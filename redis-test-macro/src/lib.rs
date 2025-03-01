use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn, parse::Parse, parse::ParseStream, Token, LitBool, Ident};

struct TestArgs {
    start_paused: bool,
}

impl Parse for TestArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut start_paused = false;
        
        if !input.is_empty() {
            let ident: Ident = input.parse()?;
            if ident != "start_paused" {
                return Err(syn::Error::new(ident.span(), "expected `start_paused`"));
            }
            input.parse::<Token![=]>()?;
            start_paused = input.parse::<LitBool>()?.value;
        }
        
        Ok(TestArgs { start_paused })
    }
}

#[proc_macro_attribute]
pub fn redis_test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as TestArgs);
    let input = parse_macro_input!(item as ItemFn);
    let fn_name = &input.sig.ident;
    let body = &input.block;
    let start_paused = args.start_paused;

    let expanded = quote! {
        #[tokio::test(start_paused = #start_paused)]
        async fn #fn_name() {
            let _guard = crate::test_utils::sync::TEST_MUTEX.lock().await;
            #body
        }
    };

    TokenStream::from(expanded)
} 