//! Procedural macros for type-safe Toasty query construction.

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

mod query_spec;
mod tc_query;

/// Generate optional field-oriented methods for a Toasty model query builder.
#[proc_macro_derive(TcQuery, attributes(tc_query))]
pub fn derive_tc_query(input: TokenStream) -> TokenStream {
    tc_query::expand(parse_macro_input!(input as DeriveInput))
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Generate an independent dynamic query request type from a query specification.
#[proc_macro]
pub fn tc_query_spec(input: TokenStream) -> TokenStream {
    query_spec::expand(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
