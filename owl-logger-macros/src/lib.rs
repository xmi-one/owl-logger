use proc_macro::TokenStream;
use proc_macro_crate::{crate_name, FoundCrate};
use quote::quote;
use syn::ext::IdentExt;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{
    parse_macro_input, FnArg, Ident, ItemFn, Meta, MetaList, MetaNameValue, Pat, ReturnType, Token,
    Type,
};

/// # `#[monitor]` — 函数监控宏
///
/// 自动为函数注入入口/出口日志和执行耗时统计。
/// 如果函数返回 Result 且为 Err，将自动升级日志级别为 ERROR，并附带错误详情。
///
/// ## 参数
///
/// - `level = "debug"`：监控日志级别（默认 `info`）。
/// - `target = "auth"`：自定义日志 `target`（默认 `"monitor"`）。
/// - `module_target`（或 `module_target = true`）：使用函数所在模块的 `module_path!()` 作为 `target`，便于配合 `RUST_LOG="my_crate=debug"` 白名单过滤。
/// - `skip(self, a, b)`：省略指定参数，输出为 `[REDACTED]`（编译期校验参数名是否存在，支持 `self`）。
/// - `skip_all`：不记录任何参数，避免参数必须实现 `Debug`。
/// - `skip_return`：不记录返回值，也不进行 Result::Err 自动升级。
/// - `slow_ms = 200`：超过该毫秒数时以 WARN 级别标记 `SLOW`。
/// - `max_len = 1024`：单个参数或返回值 `Debug` 文本的最大字节长度，超出截断追加 `...`（默认 `1024`，`0` 表示不限）。
/// - `span`（或 `span = true`）：为函数建立一个 `tracing::Span`，
///   使入口/出口/异常日志以及函数体内部的所有日志自动归属于以函数名命名的上下文。
///
/// ## 性能
///
/// 当目标日志在可能触发的级别均被过滤时，参数 `Debug` 格式化与进入/退出日志逻辑都会被跳过，实现近似零开销。
#[proc_macro_attribute]
pub fn monitor(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let args = match syn::parse::<MonitorArgs>(attr) {
        Ok(args) => args,
        Err(error) => return error.into_compile_error().into(),
    };

    if let Err(error) = validate_skip_idents(&input.sig.inputs, &args.skip) {
        return error.into_compile_error().into();
    }

    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let vis = &input.vis;
    let fn_attrs = &input.attrs;
    let sig = &input.sig;
    let body = &input.block;
    let return_type = &input.sig.output;
    let is_async = input.sig.asyncness.is_some();
    let use_span = args.span;
    let max_len = args.max_len.unwrap_or(1024);
    let logger_crate = owl_logger_crate_path();

    let target_token = if let Some(custom_target) = &args.target {
        quote! { #custom_target }
    } else if args.module_target {
        quote! { ::core::module_path!() }
    } else {
        quote! { "monitor" }
    };

    let skip_names: Vec<String> = args.skip.iter().map(Ident::to_string).collect();
    let param_formats = if args.skip_all {
        quote! { String::new() }
    } else {
        build_param_formats(&input.sig.inputs, &skip_names, max_len, &logger_crate)
    };

    // 构建日志级别的 token（全部走 owl-logger re-export 路径，避免用户必须显式依赖 tracing）
    let level_str = args.level.unwrap_or_else(|| "info".to_string());
    let level_token = match monitor_level_token(&level_str, &logger_crate) {
        Ok(level) => level,
        Err(error) => return error.into_compile_error().into(),
    };
    let slow_check = if let Some(ms) = args.slow_ms {
        quote! { __owl_elapsed >= std::time::Duration::from_millis(#ms) }
    } else {
        quote! { false }
    };

    let has_return = !matches!(return_type, ReturnType::Default);
    let may_be_result = has_return && !args.skip_return && return_type_may_be_result(return_type);

    let exit_log = if has_return && args.skip_return {
        quote! {
            if __owl_slow {
                if #logger_crate::__private::tracing::enabled!(
                    target: #target_token,
                    #logger_crate::__private::tracing::Level::WARN
                ) {
                    let __owl_lang = #logger_crate::__private::get_language();
                    let __owl_exiting = #logger_crate::__private::I18n::exiting_function(__owl_lang);
                    let __owl_elapsed_label = #logger_crate::__private::I18n::elapsed(__owl_lang);
                    #logger_crate::__private::tracing::event!(
                        target: #target_token,
                        #logger_crate::__private::tracing::Level::WARN,
                        "{} {}({}) — {} {:.2?} — SLOW — return value omitted",
                        __owl_exiting,
                        #fn_name_str,
                        __owl_params,
                        __owl_elapsed_label,
                        __owl_elapsed
                    );
                }
            } else if __owl_level_enabled {
                let __owl_lang = #logger_crate::__private::get_language();
                let __owl_exiting = #logger_crate::__private::I18n::exiting_function(__owl_lang);
                let __owl_elapsed_label = #logger_crate::__private::I18n::elapsed(__owl_lang);
                #logger_crate::__private::tracing::event!(
                    target: #target_token,
                    #level_token,
                    "{} {}({}) — {} {:.2?} — return value omitted",
                    __owl_exiting,
                    #fn_name_str,
                    __owl_params,
                    __owl_elapsed_label,
                    __owl_elapsed
                );
            }
        }
    } else if has_return {
        quote! {
            // 使用 autoref 特化在运行期动态判定是否为 Err 并自动升级日志级别
            #[allow(unused_imports)]
            use #logger_crate::__private::{OwlHighPriority, OwlLowPriority};
            let __owl_result_info =
                (&#logger_crate::__private::OwlWrap(&__owl_result)).owl_inspect_bounded(#max_len);

            if __owl_result_info.is_err {
                if #logger_crate::__private::tracing::enabled!(
                    target: #target_token,
                    #logger_crate::__private::tracing::Level::ERROR
                ) {
                    let __owl_lang = #logger_crate::__private::get_language();
                    let __owl_exiting = #logger_crate::__private::I18n::exiting_function(__owl_lang);
                    let __owl_elapsed_label = #logger_crate::__private::I18n::elapsed(__owl_lang);
                    let __owl_error_detail =
                        __owl_result_info.error_msg.as_deref().unwrap_or("unknown error");
                    #logger_crate::__private::tracing::event!(
                        target: #target_token,
                        #logger_crate::__private::tracing::Level::ERROR,
                        "{} {}({}) — {} {:.2?} — ERROR: {}",
                        __owl_exiting,
                        #fn_name_str,
                        __owl_params,
                        __owl_elapsed_label,
                        __owl_elapsed,
                        __owl_error_detail
                    );
                }
            } else if __owl_slow {
                if #logger_crate::__private::tracing::enabled!(
                    target: #target_token,
                    #logger_crate::__private::tracing::Level::WARN
                ) {
                    let __owl_lang = #logger_crate::__private::get_language();
                    let __owl_exiting = #logger_crate::__private::I18n::exiting_function(__owl_lang);
                    let __owl_elapsed_label = #logger_crate::__private::I18n::elapsed(__owl_lang);
                    let __owl_returned_label = #logger_crate::__private::I18n::returned(__owl_lang);
                    let __owl_ret_str =
                        #logger_crate::__private::format_debug_truncated(&__owl_result, #max_len);
                    #logger_crate::__private::tracing::event!(
                        target: #target_token,
                        #logger_crate::__private::tracing::Level::WARN,
                        "{} {}({}) — {} {:.2?} — SLOW — {} {}",
                        __owl_exiting,
                        #fn_name_str,
                        __owl_params,
                        __owl_elapsed_label,
                        __owl_elapsed,
                        __owl_returned_label,
                        __owl_ret_str
                    );
                }
            } else if __owl_level_enabled {
                let __owl_lang = #logger_crate::__private::get_language();
                let __owl_exiting = #logger_crate::__private::I18n::exiting_function(__owl_lang);
                let __owl_elapsed_label = #logger_crate::__private::I18n::elapsed(__owl_lang);
                let __owl_returned_label = #logger_crate::__private::I18n::returned(__owl_lang);
                let __owl_ret_str =
                    #logger_crate::__private::format_debug_truncated(&__owl_result, #max_len);
                #logger_crate::__private::tracing::event!(
                    target: #target_token,
                    #level_token,
                    "{} {}({}) — {} {:.2?} — {} {}",
                    __owl_exiting,
                    #fn_name_str,
                    __owl_params,
                    __owl_elapsed_label,
                    __owl_elapsed,
                    __owl_returned_label,
                    __owl_ret_str
                );
            }
        }
    } else {
        quote! {
            if __owl_slow {
                if #logger_crate::__private::tracing::enabled!(
                    target: #target_token,
                    #logger_crate::__private::tracing::Level::WARN
                ) {
                    let __owl_lang = #logger_crate::__private::get_language();
                    let __owl_exiting = #logger_crate::__private::I18n::exiting_function(__owl_lang);
                    let __owl_elapsed_label = #logger_crate::__private::I18n::elapsed(__owl_lang);
                    #logger_crate::__private::tracing::event!(
                        target: #target_token,
                        #logger_crate::__private::tracing::Level::WARN,
                        "{} {}({}) — {} {:.2?} — SLOW",
                        __owl_exiting,
                        #fn_name_str,
                        __owl_params,
                        __owl_elapsed_label,
                        __owl_elapsed
                    );
                }
            } else if __owl_level_enabled {
                let __owl_lang = #logger_crate::__private::get_language();
                let __owl_exiting = #logger_crate::__private::I18n::exiting_function(__owl_lang);
                let __owl_elapsed_label = #logger_crate::__private::I18n::elapsed(__owl_lang);
                #logger_crate::__private::tracing::event!(
                    target: #target_token,
                    #level_token,
                    "{} {}({}) — {} {:.2?}",
                    __owl_exiting,
                    #fn_name_str,
                    __owl_params,
                    __owl_elapsed_label,
                    __owl_elapsed
                );
            }
        }
    };

    let entering_log = quote! {
        let __owl_lang = #logger_crate::__private::get_language();
        let __owl_entering = #logger_crate::__private::I18n::entering_function(__owl_lang);
        #logger_crate::__private::tracing::event!(
            target: #target_token,
            #level_token,
            "{} {}({})",
            __owl_entering,
            #fn_name_str,
            __owl_params
        );
    };

    let return_expr = if has_return {
        quote! { __owl_result }
    } else {
        quote! {}
    };

    // 同步闭包通过 `call_once(move || ...)` 强制收敛为 `FnOnce`（与异步 `async move` 对齐），
    // 使 `&'a mut self` / `&'a mut T` 引用按值移入并原生命周期返回，不会触发 `FnMut` 捕获逃逸错误。
    let exec_expr = if is_async {
        if use_span {
            quote! {
                {
                    use #logger_crate::__private::tracing::Instrument as _;
                    async move { #body }.instrument(__owl_span.clone()).await
                }
            }
        } else {
            quote! { async move { #body }.await }
        }
    } else {
        quote! { #logger_crate::__private::call_once(move || { #body }) }
    };

    // 通过不可达的 `if false { return __owl_result; }` 在进入 Autoref 特化前，
    // 让编译器先将 `__owl_result` 与外层函数的真实返回类型（含 elided lifetime、`Self`、`impl Trait`）合一。
    let body_call = if has_return {
        quote! {
            let __owl_result = #exec_expr;
            #[allow(unreachable_code, clippy::all)]
            if false {
                return __owl_result;
            }
        }
    } else {
        quote! { let _: () = #exec_expr; }
    };

    let mut extra_capture_conditions = Vec::new();
    if args.slow_ms.is_some() {
        extra_capture_conditions.push(quote! {
            #logger_crate::__private::tracing::enabled!(
                target: #target_token,
                #logger_crate::__private::tracing::Level::WARN
            )
        });
    }
    if may_be_result {
        extra_capture_conditions.push(quote! {
            #logger_crate::__private::tracing::enabled!(
                target: #target_token,
                #logger_crate::__private::tracing::Level::ERROR
            )
        });
    }
    let should_capture_expr = if extra_capture_conditions.is_empty() {
        quote! { __owl_level_enabled }
    } else {
        quote! { __owl_level_enabled || #(#extra_capture_conditions)||* }
    };

    let expanded = if use_span && is_async {
        quote! {
            #(#fn_attrs)*
            #vis #sig {
                let __owl_level_enabled = #logger_crate::__private::tracing::enabled!(
                    target: #target_token,
                    #level_token
                );
                let __owl_should_capture = #should_capture_expr;
                let __owl_params: String = if __owl_should_capture {
                    #param_formats
                } else {
                    String::new()
                };

                let __owl_span = #logger_crate::__private::tracing::span!(
                    target: #target_token,
                    #level_token,
                    #fn_name_str
                );
                if __owl_level_enabled {
                    __owl_span.in_scope(|| {
                        #entering_log
                    });
                }

                let __owl_start = std::time::Instant::now();
                #body_call
                #[allow(unreachable_code)]
                {
                    let __owl_elapsed = __owl_start.elapsed();
                    let __owl_slow = #slow_check;

                    if __owl_should_capture {
                        __owl_span.in_scope(|| {
                            #exit_log
                        });
                    }

                    drop(__owl_span);
                    #return_expr
                }
            }
        }
    } else if use_span {
        quote! {
            #(#fn_attrs)*
            #vis #sig {
                let __owl_level_enabled = #logger_crate::__private::tracing::enabled!(
                    target: #target_token,
                    #level_token
                );
                let __owl_should_capture = #should_capture_expr;
                let __owl_params: String = if __owl_should_capture {
                    #param_formats
                } else {
                    String::new()
                };

                let __owl_span = #logger_crate::__private::tracing::span!(
                    target: #target_token,
                    #level_token,
                    #fn_name_str
                );
                let __owl_span_guard = __owl_span.enter();
                if __owl_level_enabled {
                    #entering_log
                }

                let __owl_start = std::time::Instant::now();
                #body_call
                #[allow(unreachable_code)]
                {
                    let __owl_elapsed = __owl_start.elapsed();
                    let __owl_slow = #slow_check;

                    if __owl_should_capture {
                        #exit_log
                    }

                    drop(__owl_span_guard);
                    #return_expr
                }
            }
        }
    } else {
        quote! {
            #(#fn_attrs)*
            #vis #sig {
                let __owl_level_enabled = #logger_crate::__private::tracing::enabled!(
                    target: #target_token,
                    #level_token
                );
                let __owl_should_capture = #should_capture_expr;
                let __owl_params: String = if __owl_should_capture {
                    #param_formats
                } else {
                    String::new()
                };

                if __owl_level_enabled {
                    #entering_log
                }

                let __owl_start = std::time::Instant::now();
                #body_call
                #[allow(unreachable_code)]
                {
                    let __owl_elapsed = __owl_start.elapsed();
                    let __owl_slow = #slow_check;

                    if __owl_should_capture {
                        #exit_log
                    }

                    #return_expr
                }
            }
        }
    };

    TokenStream::from(expanded)
}

fn owl_logger_crate_path() -> proc_macro2::TokenStream {
    match crate_name("owl-logger") {
        Ok(FoundCrate::Itself) => quote! { ::owl_logger },
        Ok(FoundCrate::Name(name)) => {
            let ident = Ident::new(&name.replace('-', "_"), proc_macro2::Span::call_site());
            quote! { ::#ident }
        }
        Err(_) => quote! { ::owl_logger },
    }
}

fn monitor_level_token(
    level: &str,
    logger_crate: &proc_macro2::TokenStream,
) -> syn::Result<proc_macro2::TokenStream> {
    match level {
        "trace" => Ok(quote! { #logger_crate::__private::tracing::Level::TRACE }),
        "debug" => Ok(quote! { #logger_crate::__private::tracing::Level::DEBUG }),
        "info" => Ok(quote! { #logger_crate::__private::tracing::Level::INFO }),
        "warn" => Ok(quote! { #logger_crate::__private::tracing::Level::WARN }),
        "error" => Ok(quote! { #logger_crate::__private::tracing::Level::ERROR }),
        _ => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "level must be one of: trace, debug, info, warn, error",
        )),
    }
}

/// 静态判断返回类型在语法上是否有可能是 `Result<T, E>`（包含类型别名如 `anyhow::Result<T>`）。
fn return_type_may_be_result(ret: &ReturnType) -> bool {
    let ReturnType::Type(_, ty) = ret else {
        return false;
    };
    type_may_be_result(ty)
}

fn type_may_be_result(ty: &Type) -> bool {
    match ty {
        Type::Paren(inner) => type_may_be_result(&inner.elem),
        Type::Group(inner) => type_may_be_result(&inner.elem),
        Type::Tuple(_)
        | Type::Reference(_)
        | Type::Slice(_)
        | Type::Array(_)
        | Type::Ptr(_)
        | Type::Never(_)
        | Type::BareFn(_)
        | Type::TraitObject(_) => false,
        Type::Path(type_path) if type_path.qself.is_none() => {
            if let Some(ident) = type_path.path.get_ident() {
                let name = ident.to_string();
                !matches!(
                    name.as_str(),
                    "bool"
                        | "char"
                        | "str"
                        | "u8"
                        | "u16"
                        | "u32"
                        | "u64"
                        | "u128"
                        | "usize"
                        | "i8"
                        | "i16"
                        | "i32"
                        | "i64"
                        | "i128"
                        | "isize"
                        | "f32"
                        | "f64"
                )
            } else {
                true
            }
        }
        _ => true,
    }
}

/// 过程宏属性参数
struct MonitorArgs {
    level: Option<String>,
    target: Option<String>,
    module_target: bool,
    skip: Vec<Ident>,
    skip_all: bool,
    skip_return: bool,
    slow_ms: Option<u64>,
    max_len: Option<usize>,
    span: bool,
}

impl Parse for MonitorArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut level = None;
        let mut target: Option<String> = None;
        let mut module_target = false;
        let mut skip = Vec::new();
        let mut skip_all = false;
        let mut skip_return = false;
        let mut slow_ms = None;
        let mut max_len = None;
        let mut span = false;

        if input.is_empty() {
            return Ok(MonitorArgs {
                level,
                target,
                module_target,
                skip,
                skip_all,
                skip_return,
                slow_ms,
                max_len,
                span,
            });
        }

        let nested = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
        for meta in nested {
            match meta {
                Meta::NameValue(MetaNameValue { path, value, .. }) => {
                    if path.is_ident("level") {
                        if let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(lit_str),
                            ..
                        }) = value
                        {
                            level = Some(lit_str.value().to_lowercase());
                        } else {
                            return Err(syn::Error::new_spanned(
                                value,
                                "level must be a string literal, e.g., level = \"debug\"",
                            ));
                        }
                    } else if path.is_ident("target") {
                        if let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(lit_str),
                            ..
                        }) = value
                        {
                            let val = lit_str.value();
                            if val.is_empty() {
                                return Err(syn::Error::new_spanned(
                                    lit_str,
                                    "target cannot be empty",
                                ));
                            }
                            target = Some(val);
                        } else {
                            return Err(syn::Error::new_spanned(
                                value,
                                "target must be a string literal, e.g., target = \"auth\"",
                            ));
                        }
                    } else if path.is_ident("module_target") {
                        if let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Bool(lit_bool),
                            ..
                        }) = value
                        {
                            module_target = lit_bool.value;
                        } else {
                            return Err(syn::Error::new_spanned(
                                value,
                                "module_target must be a boolean literal, e.g., module_target = true",
                            ));
                        }
                    } else if path.is_ident("slow_ms") {
                        if let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Int(lit_int),
                            ..
                        }) = value
                        {
                            slow_ms = Some(lit_int.base10_parse()?);
                        } else {
                            return Err(syn::Error::new_spanned(
                                value,
                                "slow_ms must be an integer literal, e.g., slow_ms = 200",
                            ));
                        }
                    } else if path.is_ident("max_len") {
                        if let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Int(lit_int),
                            ..
                        }) = value
                        {
                            max_len = Some(lit_int.base10_parse()?);
                        } else {
                            return Err(syn::Error::new_spanned(
                                value,
                                "max_len must be an integer literal, e.g., max_len = 1024",
                            ));
                        }
                    } else if path.is_ident("span") {
                        if let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Bool(lit_bool),
                            ..
                        }) = value
                        {
                            span = lit_bool.value;
                        } else {
                            return Err(syn::Error::new_spanned(
                                value,
                                "span must be a boolean literal, e.g., span = true",
                            ));
                        }
                    } else {
                        return Err(syn::Error::new_spanned(
                            path,
                            "unknown argument, supported: level, target, module_target, skip, skip_all, skip_return, slow_ms, max_len, span",
                        ));
                    }
                }
                Meta::List(MetaList { path, tokens, .. }) => {
                    if path.is_ident("skip") {
                        let idents = syn::parse::Parser::parse2(
                            |stream: ParseStream| {
                                Punctuated::<Ident, Token![,]>::parse_terminated_with(
                                    stream,
                                    Ident::parse_any,
                                )
                            },
                            tokens,
                        )?;
                        for ident in idents {
                            skip.push(ident);
                        }
                    } else {
                        return Err(syn::Error::new_spanned(
                            path,
                            "unknown argument, supported: level, target, module_target, skip, skip_all, skip_return, slow_ms, max_len, span",
                        ));
                    }
                }
                Meta::Path(path) => {
                    if path.is_ident("span") {
                        span = true;
                    } else if path.is_ident("module_target") {
                        module_target = true;
                    } else if path.is_ident("skip_all") {
                        skip_all = true;
                    } else if path.is_ident("skip_return") {
                        skip_return = true;
                    } else {
                        return Err(syn::Error::new_spanned(
                            path,
                            "unknown argument, supported: level, target, module_target, skip, skip_all, skip_return, slow_ms, max_len, span",
                        ));
                    }
                }
            }
        }

        if target.is_some() && module_target {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "cannot specify both `target = \"...\"` and `module_target` in #[monitor]",
            ));
        }

        Ok(MonitorArgs {
            level,
            target,
            module_target,
            skip,
            skip_all,
            skip_return,
            slow_ms,
            max_len,
            span,
        })
    }
}

fn collect_pat_idents(pat: &Pat, out: &mut Vec<String>) {
    match pat {
        Pat::Ident(pat_ident) => {
            out.push(pat_ident.ident.to_string());
            if let Some((_, subpat)) = &pat_ident.subpat {
                collect_pat_idents(subpat, out);
            }
        }
        Pat::Tuple(pat_tuple) => {
            for elem in &pat_tuple.elems {
                collect_pat_idents(elem, out);
            }
        }
        Pat::TupleStruct(pat_ts) => {
            for elem in &pat_ts.elems {
                collect_pat_idents(elem, out);
            }
        }
        Pat::Struct(pat_struct) => {
            for field in &pat_struct.fields {
                collect_pat_idents(&field.pat, out);
            }
        }
        Pat::Slice(pat_slice) => {
            for elem in &pat_slice.elems {
                collect_pat_idents(elem, out);
            }
        }
        Pat::Reference(pat_ref) => collect_pat_idents(&pat_ref.pat, out),
        Pat::Type(pat_ty) => collect_pat_idents(&pat_ty.pat, out),
        Pat::Paren(pat_paren) => collect_pat_idents(&pat_paren.pat, out),
        Pat::Or(pat_or) => {
            for case in &pat_or.cases {
                collect_pat_idents(case, out);
            }
        }
        _ => {}
    }
}

fn validate_skip_idents(
    inputs: &Punctuated<FnArg, Token![,]>,
    skip_idents: &[Ident],
) -> syn::Result<()> {
    if skip_idents.is_empty() {
        return Ok(());
    }
    let mut known_params = Vec::new();
    for arg in inputs {
        match arg {
            FnArg::Receiver(_) => known_params.push("self".to_string()),
            FnArg::Typed(pat_type) => collect_pat_idents(&pat_type.pat, &mut known_params),
        }
    }
    for skip_ident in skip_idents {
        let target = skip_ident.to_string();
        if !known_params.iter().any(|p| p == &target) {
            return Err(syn::Error::new_spanned(
                skip_ident,
                format!("unknown parameter `{target}` in #[monitor(skip(...))]"),
            ));
        }
    }
    Ok(())
}

/// 构建单次缓冲区拼接的参数格式化代码，避免 N+1 次堆分配并支持长度截断保护
fn build_param_formats(
    inputs: &Punctuated<FnArg, Token![,]>,
    skip_fields: &[String],
    max_len: usize,
    logger_crate: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let mut writes: Vec<proc_macro2::TokenStream> = Vec::new();

    for arg in inputs {
        if let FnArg::Typed(pat_type) = arg {
            if let Pat::Ident(pat_ident) = &*pat_type.pat {
                let name = pat_ident.ident.to_string();
                if name == "self" {
                    continue;
                }
                let ident = &pat_ident.ident;
                let sep = if writes.is_empty() { "" } else { ", " };
                if skip_fields.contains(&name) {
                    let literal = format!("{sep}{name}=[REDACTED]");
                    writes.push(quote! { __owl_buf.push_str(#literal); });
                } else {
                    let prefix = format!("{sep}{name}=");
                    writes.push(quote! {
                        __owl_buf.push_str(#prefix);
                        #logger_crate::__private::write_debug_truncated(&mut __owl_buf, &#ident, #max_len);
                    });
                }
            }
        }
    }

    if writes.is_empty() {
        quote! { String::new() }
    } else {
        let initial_cap = writes.len() * 24;
        quote! {
            {
                let mut __owl_buf = String::with_capacity(#initial_cap);
                #(#writes)*
                __owl_buf
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        monitor_level_token, owl_logger_crate_path, return_type_may_be_result, MonitorArgs,
    };
    use syn::parse_quote;

    #[test]
    fn monitor_level_is_validated_at_macro_expansion_time() {
        let logger_crate = owl_logger_crate_path();
        assert!(monitor_level_token("info", &logger_crate).is_ok());
        assert!(monitor_level_token("warn", &logger_crate).is_ok());
        assert!(monitor_level_token("verbose", &logger_crate).is_err());
    }

    #[test]
    fn parses_skip_self_and_target_options() {
        let args: MonitorArgs = parse_quote!(skip(self, password), target = "auth");
        assert_eq!(args.skip.len(), 2);
        assert_eq!(args.skip[0].to_string(), "self");
        assert_eq!(args.target.as_deref(), Some("auth"));

        let conflict = syn::parse2::<MonitorArgs>(quote::quote!(target = "a", module_target));
        assert!(conflict.is_err());
    }

    #[test]
    fn non_result_return_types_are_identified() {
        assert!(!return_type_may_be_result(&parse_quote!()));
        assert!(!return_type_may_be_result(&parse_quote!(-> bool)));
        assert!(!return_type_may_be_result(&parse_quote!(-> usize)));
        assert!(!return_type_may_be_result(&parse_quote!(-> &str)));
        assert!(!return_type_may_be_result(&parse_quote!(-> (i32, i32))));
        assert!(return_type_may_be_result(
            &parse_quote!(-> Result<i32, String>)
        ));
        assert!(return_type_may_be_result(
            &parse_quote!(-> anyhow::Result<()>)
        ));
    }
}
