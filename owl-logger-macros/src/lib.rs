use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, FnArg, ItemFn, Pat, ReturnType};

/// # `#[monitor]` — 函数监控宏
///
/// 自动为函数注入入口/出口日志和执行耗时统计。
/// 借鉴 Python xmi_logger 的 `@log_decorator()` 装饰器。
///
/// ## 基础用法
///
/// ```rust,ignore
/// #[owl_logger::monitor]
/// fn process_order(order_id: &str, amount: f64) -> bool {
///     // 业务逻辑...
///     true
/// }
/// // 自动输出：
/// // INFO → entering process_order(order_id="ORD-001", amount=99.9)
/// // INFO ← exiting process_order — elapsed 1.23ms — returned true
/// ```
///
/// ## 异步函数
///
/// ```rust,ignore
/// #[owl_logger::monitor]
/// async fn fetch_data(url: &str) -> Result<String, Error> {
///     // ...
/// }
/// ```
///
/// ## 自定义级别和跳过参数
///
/// ```rust,ignore
/// #[owl_logger::monitor(level = "debug", skip(password))]
/// fn login(username: &str, password: &str) -> bool {
///     true
/// }
/// ```
#[proc_macro_attribute]
pub fn monitor(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let attrs = attr.to_string();

    // 解析属性参数
    let level = parse_level(&attrs);
    let skip_fields = parse_skip(&attrs);

    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let is_async = input.sig.asyncness.is_some();
    let vis = &input.vis;
    let fn_attrs = &input.attrs;
    let sig = &input.sig;
    let body = &input.block;
    let return_type = &input.sig.output;

    // 收集参数名和格式化
    let param_formats = build_param_formats(&input.sig.inputs, &skip_fields);

    // 构建日志级别的 token
    let level_token = match level.as_str() {
        "trace" => quote! { tracing::Level::TRACE },
        "debug" => quote! { tracing::Level::DEBUG },
        "warn" => quote! { tracing::Level::WARN },
        "error" => quote! { tracing::Level::ERROR },
        _ => quote! { tracing::Level::INFO },
    };

    let has_return = !matches!(return_type, ReturnType::Default);

    let exit_log = if has_return {
        quote! {
            tracing::event!(#level_token, "← exiting {}({}) — elapsed {:.2?} — returned {:?}", #fn_name_str, #param_formats, __owl_elapsed, __owl_result);
        }
    } else {
        quote! {
            tracing::event!(#level_token, "← exiting {}({}) — elapsed {:.2?}", #fn_name_str, #param_formats, __owl_elapsed);
        }
    };

    let return_expr = if has_return {
        quote! { __owl_result }
    } else {
        quote! {}
    };

    let body_call = if has_return {
        quote! { let __owl_result = { #body }; }
    } else {
        quote! { { #body } }
    };

    let expanded = if is_async {
        quote! {
            #(#fn_attrs)*
            #vis #sig {
                tracing::event!(#level_token, "→ entering {}({})", #fn_name_str, #param_formats);
                let __owl_start = std::time::Instant::now();
                #body_call
                let __owl_elapsed = __owl_start.elapsed();
                #exit_log
                #return_expr
            }
        }
    } else {
        quote! {
            #(#fn_attrs)*
            #vis #sig {
                tracing::event!(#level_token, "→ entering {}({})", #fn_name_str, #param_formats);
                let __owl_start = std::time::Instant::now();
                #body_call
                let __owl_elapsed = __owl_start.elapsed();
                #exit_log
                #return_expr
            }
        }
    };

    TokenStream::from(expanded)
}

/// 解析 level 参数，如 `level = "debug"`
fn parse_level(attrs: &str) -> String {
    if let Some(pos) = attrs.find("level") {
        let rest = &attrs[pos..];
        if let Some(start) = rest.find('"') {
            let rest = &rest[start + 1..];
            if let Some(end) = rest.find('"') {
                return rest[..end].to_lowercase();
            }
        }
    }
    "info".to_string()
}

/// 解析 skip 参数列表，如 `skip(password, secret)`
fn parse_skip(attrs: &str) -> Vec<String> {
    if let Some(pos) = attrs.find("skip(") {
        let rest = &attrs[pos + 5..];
        if let Some(end) = rest.find(')') {
            return rest[..end]
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    Vec::new()
}

/// 构建参数格式化字符串
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

