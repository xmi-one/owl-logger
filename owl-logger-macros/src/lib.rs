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

    // 收集参数名和格式化
    let param_formats = build_param_formats(&input.sig.inputs, &args.skip);

    // 构建日志级别的 token
    let level_str = args.level.unwrap_or_else(|| "info".to_string());
    let level_token = match level_str.as_str() {
        "trace" => quote! { tracing::Level::TRACE },
        "debug" => quote! { tracing::Level::DEBUG },
        "warn" => quote! { tracing::Level::WARN },
        "error" => quote! { tracing::Level::ERROR },
        _ => quote! { tracing::Level::INFO },
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
                tracing::event!(
                    target: "monitor",
                    tracing::Level::ERROR,
                    "{} {}({}) — {} {:.2?} — ERROR: {}",
                    __owl_exiting,
                    #fn_name_str,
                    __owl_params,
                    __owl_elapsed_label,
                    __owl_elapsed,
                    __owl_error_detail
                );
            } else if __owl_slow {
                tracing::event!(
                    target: "monitor",
                    tracing::Level::WARN,
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
                tracing::event!(
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
                tracing::event!(
                    target: "monitor",
                    tracing::Level::WARN,
                    "{} {}({}) — {} {:.2?} — SLOW",
                    __owl_exiting,
                    #fn_name_str,
                    __owl_params,
                    __owl_elapsed_label,
                    __owl_elapsed
                );
            } else {
                tracing::event!(
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
        tracing::event!(
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

    let body_call = if is_async {
        if has_return {
            quote! {
                let __owl_result = async move {
                    #body
                }.await;
            }
        } else {
            quote! {
                async move {
                    #body
                }.await;
            }
        }
    } else if has_return {
        quote! {
            let __owl_result = (|| {
                #body
            })();
        }
    } else {
        quote! {
            (|| {
                #body
            })();
        }
    };

    let expanded = quote! {
        #(#fn_attrs)*
        #vis #sig {
            let __owl_params = #param_formats;
            #entering_log
            let __owl_start = std::time::Instant::now();
            #body_call
            let __owl_elapsed = __owl_start.elapsed();
            let __owl_slow = #slow_check;
            #exit_log
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
}

impl Parse for MonitorArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut level = None;
        let mut skip = Vec::new();
        let mut slow_ms = None;

        if input.is_empty() {
            return Ok(MonitorArgs {
                level,
                skip,
                slow_ms,
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
                    } else {
                        return Err(syn::Error::new_spanned(
                            path,
                            "unknown argument, supported: level, skip, slow_ms",
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
                            "unknown argument, supported: level, skip, slow_ms",
                        ));
                    }
                }
                Meta::Path(path) => {
                    return Err(syn::Error::new_spanned(path, "unknown argument style"));
                }
            }
        }

        Ok(MonitorArgs {
            level,
            skip,
            slow_ms,
        })
    }
}

/// 构建参数格式化代码
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
        quote! { "" }
    } else {
        quote! {
            &[#(#param_strings),*].join(", ")
        }
    }
}
