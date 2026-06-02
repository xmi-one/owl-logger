use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{
    parse_macro_input, FnArg, Ident, ItemFn, Meta, MetaList, MetaNameValue, Pat, ReturnType, Token,
};

/// # `#[monitor]` — 函数监控宏
///
/// 自动为函数注入入口/出口日志和执行耗时统计。
/// 如果函数返回 Result 且为 Err，将自动升级日志级别为 ERROR，并附带错误详情。
///
/// ## 参数
///
/// - `level = "debug"`：监控日志级别（默认 `info`）。
/// - `skip(a, b)`：脱敏指定参数，输出为 `[REDACTED]`。
/// - `slow_ms = 200`：超过该毫秒数时以 WARN 级别标记 `SLOW`。
/// - `span`（或 `span = true`）：为函数体建立一个 `tracing::Span`，
///   使函数内部的所有日志自动带上以函数名命名的上下文。
///
/// ## 性能
///
/// 当 `target: "monitor"` 的日志被完全过滤时，参数 `Debug` 格式化与
/// 进入/退出日志逻辑都会被跳过，实现近似零开销。
#[proc_macro_attribute]
pub fn monitor(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    // 使用 syn 解析宏属性参数
    let args = parse_macro_input!(attr as MonitorArgs);

    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let vis = &input.vis;
    let fn_attrs = &input.attrs;
    let sig = &input.sig;
    let body = &input.block;
    let return_type = &input.sig.output;
    let is_async = input.sig.asyncness.is_some();
    let use_span = args.span;

    // 收集参数名和格式化
    let param_formats = build_param_formats(&input.sig.inputs, &args.skip);

    // 构建日志级别的 token（全部走 owl_logger re-export 路径，避免用户必须显式依赖 tracing）
    let level_str = args.level.unwrap_or_else(|| "info".to_string());
    let level_token = match level_str.as_str() {
        "trace" => quote! { owl_logger::__private::tracing::Level::TRACE },
        "debug" => quote! { owl_logger::__private::tracing::Level::DEBUG },
        "warn" => quote! { owl_logger::__private::tracing::Level::WARN },
        "error" => quote! { owl_logger::__private::tracing::Level::ERROR },
        _ => quote! { owl_logger::__private::tracing::Level::INFO },
    };
    let slow_check = if let Some(ms) = args.slow_ms {
        quote! { __owl_elapsed >= std::time::Duration::from_millis(#ms) }
    } else {
        quote! { false }
    };

    let has_return = !matches!(return_type, ReturnType::Default);

    let exit_log = if has_return {
        quote! {
            let __owl_lang = owl_logger::__private::get_language();
            let __owl_exiting = owl_logger::__private::I18n::exiting_function(__owl_lang);
            let __owl_elapsed_label = owl_logger::__private::I18n::elapsed(__owl_lang);
            let __owl_returned_label = owl_logger::__private::I18n::returned(__owl_lang);

            // 使用 autoref 特化在运行期动态判定是否为 Err 并自动升级日志级别
            #[allow(unused_imports)]
            use owl_logger::__private::{OwlLowPriority, OwlHighPriority};
            let __owl_result_info = (&owl_logger::__private::OwlWrap(&__owl_result)).owl_inspect();

            if __owl_result_info.is_err {
                let __owl_error_detail = __owl_result_info.error_msg.as_deref().unwrap_or("unknown error");
                owl_logger::__private::tracing::event!(
                    target: "monitor",
                    owl_logger::__private::tracing::Level::ERROR,
                    "{} {}({}) — {} {:.2?} — ERROR: {}",
                    __owl_exiting,
                    #fn_name_str,
                    __owl_params,
                    __owl_elapsed_label,
                    __owl_elapsed,
                    __owl_error_detail
                );
            } else if __owl_slow {
                owl_logger::__private::tracing::event!(
                    target: "monitor",
                    owl_logger::__private::tracing::Level::WARN,
                    "{} {}({}) — {} {:.2?} — SLOW — {} {:?}",
                    __owl_exiting,
                    #fn_name_str,
                    __owl_params,
                    __owl_elapsed_label,
                    __owl_elapsed,
                    __owl_returned_label,
                    __owl_result
                );
            } else {
                owl_logger::__private::tracing::event!(
                    target: "monitor",
                    #level_token,
                    "{} {}({}) — {} {:.2?} — {} {:?}",
                    __owl_exiting,
                    #fn_name_str,
                    __owl_params,
                    __owl_elapsed_label,
                    __owl_elapsed,
                    __owl_returned_label,
                    __owl_result
                );
            }
        }
    } else {
        quote! {
            let __owl_lang = owl_logger::__private::get_language();
            let __owl_exiting = owl_logger::__private::I18n::exiting_function(__owl_lang);
            let __owl_elapsed_label = owl_logger::__private::I18n::elapsed(__owl_lang);
            if __owl_slow {
                owl_logger::__private::tracing::event!(
                    target: "monitor",
                    owl_logger::__private::tracing::Level::WARN,
                    "{} {}({}) — {} {:.2?} — SLOW",
                    __owl_exiting,
                    #fn_name_str,
                    __owl_params,
                    __owl_elapsed_label,
                    __owl_elapsed
                );
            } else {
                owl_logger::__private::tracing::event!(
                    target: "monitor",
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
        let __owl_lang = owl_logger::__private::get_language();
        let __owl_entering = owl_logger::__private::I18n::entering_function(__owl_lang);
        owl_logger::__private::tracing::event!(
            target: "monitor",
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

    // 函数体执行表达式（始终用闭包/async 块包裹，以正确处理函数体内的提前 return）
    let exec_expr = if is_async {
        if use_span {
            quote! {
                {
                    use owl_logger::__private::tracing::Instrument as _;
                    async move { #body }.instrument(__owl_span).await
                }
            }
        } else {
            quote! { async move { #body }.await }
        }
    } else if use_span {
        quote! {
            {
                let __owl_enter = __owl_span.enter();
                (|| { #body })()
            }
        }
    } else {
        quote! { (|| { #body })() }
    };

    let body_call = if has_return {
        quote! { let __owl_result = #exec_expr; }
    } else {
        quote! { let _: () = #exec_expr; }
    };

    // 可选：为函数体建立 span（即使监控 enter/exit 日志被过滤，span 仍可用于内部日志上下文）
    let span_def = if use_span {
        quote! {
            let __owl_span = owl_logger::__private::tracing::span!(
                target: "monitor",
                #level_token,
                #fn_name_str
            );
        }
    } else {
        quote! {}
    };

    let expanded = quote! {
        #(#fn_attrs)*
        #vis #sig {
            // 仅当 monitor 目标在任一可能输出的级别启用时才捕获参数，否则近似零开销
            let __owl_should_log =
                owl_logger::__private::tracing::enabled!(target: "monitor", #level_token)
                || owl_logger::__private::tracing::enabled!(target: "monitor", owl_logger::__private::tracing::Level::WARN)
                || owl_logger::__private::tracing::enabled!(target: "monitor", owl_logger::__private::tracing::Level::ERROR);

            let __owl_params: String = if __owl_should_log { #param_formats } else { String::new() };

            if __owl_should_log {
                #entering_log
            }

            #span_def
            let __owl_start = std::time::Instant::now();
            #body_call
            let __owl_elapsed = __owl_start.elapsed();
            let __owl_slow = #slow_check;

            if __owl_should_log {
                #exit_log
            }

            #return_expr
        }
    };

    TokenStream::from(expanded)
}

/// 过程宏属性参数
struct MonitorArgs {
    level: Option<String>,
    skip: Vec<String>,
    slow_ms: Option<u64>,
    span: bool,
}

impl Parse for MonitorArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut level = None;
        let mut skip = Vec::new();
        let mut slow_ms = None;
        let mut span = false;

        if input.is_empty() {
            return Ok(MonitorArgs {
                level,
                skip,
                slow_ms,
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
                            "unknown argument, supported: level, skip, slow_ms, span",
                        ));
                    }
                }
                Meta::List(MetaList { path, tokens, .. }) => {
                    if path.is_ident("skip") {
                        let idents = syn::parse::Parser::parse2(
                            Punctuated::<Ident, Token![,]>::parse_terminated,
                            tokens,
                        )?;
                        for ident in idents {
                            skip.push(ident.to_string());
                        }
                    } else {
                        return Err(syn::Error::new_spanned(
                            path,
                            "unknown argument, supported: level, skip, slow_ms, span",
                        ));
                    }
                }
                Meta::Path(path) => {
                    if path.is_ident("span") {
                        span = true;
                    } else {
                        return Err(syn::Error::new_spanned(
                            path,
                            "unknown argument, supported: level, skip, slow_ms, span",
                        ));
                    }
                }
            }
        }

        Ok(MonitorArgs {
            level,
            skip,
            slow_ms,
            span,
        })
    }
}

/// 构建参数格式化代码，返回一个 `String` 表达式
fn build_param_formats(
    inputs: &syn::punctuated::Punctuated<FnArg, syn::token::Comma>,
    skip_fields: &[String],
) -> proc_macro2::TokenStream {
    let param_strings: Vec<proc_macro2::TokenStream> = inputs
        .iter()
        .filter_map(|arg| {
            if let FnArg::Typed(pat_type) = arg {
                if let Pat::Ident(pat_ident) = &*pat_type.pat {
                    let name = pat_ident.ident.to_string();
                    let ident = &pat_ident.ident;
                    if skip_fields.contains(&name) {
                        Some(quote! { format!("{}=[REDACTED]", #name) })
                    } else {
                        Some(quote! { format!("{}={:?}", #name, #ident) })
                    }
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    if param_strings.is_empty() {
        quote! { String::new() }
    } else {
        quote! {
            [#(#param_strings),*].join(", ")
        }
    }
}
