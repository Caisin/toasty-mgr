use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{
    Expr, Ident, Token, Type, Visibility, braced, bracketed,
    parse::{Parse, ParseStream},
    parse2,
};

use super::tc_query::runtime_crate_path;

pub(crate) fn expand(tokens: TokenStream) -> syn::Result<TokenStream> {
    let input = parse2::<QuerySpec>(tokens)?;
    input.validate()?;
    input.expand()
}

struct QuerySpec {
    vis: Visibility,
    name: Ident,
    model: Ident,
    filters: Vec<Filter>,
    sorts: Vec<Ident>,
    default_order: Vec<Order>,
    tie_breaker: Option<Order>,
    page: Option<PageConfig>,
}

struct Filter {
    name: Ident,
    ty: Type,
    field: Ident,
    operation: FilterOperation,
}

struct Order {
    field: Ident,
    direction: Direction,
}

struct PageConfig {
    default_size: Expr,
    max_size: Expr,
}

#[derive(Clone, Copy)]
enum Direction {
    Asc,
    Desc,
}

#[derive(Clone, Copy)]
enum FilterOperation {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Between,
    InList,
    StartsWith,
}

impl Parse for QuerySpec {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let vis = input.parse()?;
        let name = input.parse()?;
        input.parse::<Token![for]>()?;
        let model = input.parse()?;

        let body;
        braced!(body in input);

        parse_keyword(&body, "filters")?;
        let filters_body;
        braced!(filters_body in body);
        let mut filters = Vec::new();
        while !filters_body.is_empty() {
            let name: Ident = filters_body.parse()?;
            filters_body.parse::<Token![:]>()?;
            let ty = filters_body.parse()?;
            filters_body.parse::<Token![=>]>()?;
            let field = filters_body.parse()?;
            filters_body.parse::<Token![.]>()?;
            let operation = FilterOperation::parse(filters_body.parse()?)?;
            filters_body.parse::<Token![;]>()?;
            filters.push(Filter {
                name,
                ty,
                field,
                operation,
            });
        }

        let mut sorts = Vec::new();
        if next_is_keyword(&body, "sort") {
            parse_keyword(&body, "sort")?;
            let sorts_body;
            braced!(sorts_body in body);
            while !sorts_body.is_empty() {
                sorts.push(sorts_body.parse()?);
                sorts_body.parse::<Token![;]>()?;
            }
        }

        let mut default_order = Vec::new();
        if next_is_keyword(&body, "default_order") {
            parse_keyword(&body, "default_order")?;
            let defaults_body;
            bracketed!(defaults_body in body);
            while !defaults_body.is_empty() {
                default_order.push(parse_order(&defaults_body)?);
                if defaults_body.is_empty() {
                    break;
                }
                defaults_body.parse::<Token![,]>()?;
            }
            body.parse::<Token![;]>()?;
        }

        let tie_breaker = if next_is_keyword(&body, "tie_breaker") {
            parse_keyword(&body, "tie_breaker")?;
            let order = parse_order(&body)?;
            body.parse::<Token![;]>()?;
            Some(order)
        } else {
            None
        };

        let page = if next_is_keyword(&body, "page") {
            parse_keyword(&body, "page")?;
            let page_body;
            braced!(page_body in body);
            parse_keyword(&page_body, "default_size")?;
            page_body.parse::<Token![:]>()?;
            let default_size = page_body.parse()?;
            page_body.parse::<Token![;]>()?;
            parse_keyword(&page_body, "max_size")?;
            page_body.parse::<Token![:]>()?;
            let max_size = page_body.parse()?;
            page_body.parse::<Token![;]>()?;
            if !page_body.is_empty() {
                return Err(page_body.error("unexpected token in page block"));
            }
            Some(PageConfig {
                default_size,
                max_size,
            })
        } else {
            None
        };

        if !body.is_empty() {
            return Err(body.error("unexpected token in query specification"));
        }
        if !input.is_empty() {
            return Err(input.error("unexpected token after query specification"));
        }

        Ok(Self {
            vis,
            name,
            model,
            filters,
            sorts,
            default_order,
            tie_breaker,
            page,
        })
    }
}

impl QuerySpec {
    fn validate(&self) -> syn::Result<()> {
        let mut errors = None;
        check_unique(
            self.filters.iter().map(|filter| &filter.name),
            "duplicate query filter",
            &mut errors,
        );
        check_unique(self.sorts.iter(), "duplicate query sort field", &mut errors);
        check_unique(
            self.default_order.iter().map(|order| &order.field),
            "duplicate default sort field",
            &mut errors,
        );

        let sort_names = self
            .sorts
            .iter()
            .map(Ident::to_string)
            .collect::<HashSet<_>>();
        for order in &self.default_order {
            if !sort_names.contains(&order.field.to_string()) {
                combine_error(
                    &mut errors,
                    syn::Error::new_spanned(
                        &order.field,
                        "default_order field must be declared in sort",
                    ),
                );
            }
        }
        if let Some(tie_breaker) = &self.tie_breaker
            && !sort_names.contains(&tie_breaker.field.to_string())
        {
            combine_error(
                &mut errors,
                syn::Error::new_spanned(
                    &tie_breaker.field,
                    "tie_breaker field must be declared in sort",
                ),
            );
        }

        let mut method_names = self
            .filters
            .iter()
            .map(|filter| filter.name.to_string())
            .collect::<HashSet<_>>();
        for reserved in ["filter", "extra_filters", "orders"] {
            if !method_names.insert(reserved.to_owned()) {
                combine_error(
                    &mut errors,
                    syn::Error::new(self.name.span(), format!("`{reserved}` is reserved")),
                );
            }
        }
        if self.page.is_some() {
            for reserved in ["page", "size"] {
                if !method_names.insert(reserved.to_owned()) {
                    combine_error(
                        &mut errors,
                        syn::Error::new(self.name.span(), format!("`{reserved}` is reserved")),
                    );
                }
            }
        }
        for sort in &self.sorts {
            for method in [format!("asc_{sort}"), format!("desc_{sort}")] {
                if !method_names.insert(method.clone()) {
                    combine_error(
                        &mut errors,
                        syn::Error::new_spanned(
                            sort,
                            format!("generated method `{method}` conflicts"),
                        ),
                    );
                }
            }
        }

        errors.map_or(Ok(()), Err)
    }

    fn expand(&self) -> syn::Result<TokenStream> {
        let root = runtime_crate_path()?;
        let vis = &self.vis;
        let name = &self.name;
        let model = &self.model;
        let builder = format_ident!("{}Builder", name);
        let builder_module = format_ident!("{}_builder", to_snake_case(&name.to_string()));
        let order_type = format_ident!("{}Order", name);

        let filter_fields = self.filters.iter().map(|filter| {
            let name = &filter.name;
            let ty = &filter.ty;
            quote!(#name: Option<#ty>,)
        });
        let page_fields = self.page.iter().map(|page| {
            let default_size = &page.default_size;
            quote! {
                #[builder(default = 1)]
                page: u64,
                #[builder(default = #default_size)]
                size: u64,
            }
        });
        let order_variants = self.sorts.iter().map(|field| {
            let variant = order_variant(field);
            quote!(#variant(#root::query::TcQuerySortDirection),)
        });
        let order_field_arms = self.sorts.iter().map(|field| {
            let variant = order_variant(field);
            quote!(Self::#variant(_) => stringify!(#field),)
        });
        let builder_sort_methods = self.sorts.iter().map(|field| {
            let variant = order_variant(field);
            let asc = format_ident!("asc_{}", field);
            let desc = format_ident!("desc_{}", field);
            quote! {
                #vis fn #asc(mut self) -> Self {
                    self.orders.push(#order_type::#variant(
                        #root::query::TcQuerySortDirection::Asc,
                    ));
                    self
                }

                #vis fn #desc(mut self) -> Self {
                    self.orders.push(#order_type::#variant(
                        #root::query::TcQuerySortDirection::Desc,
                    ));
                    self
                }
            }
        });
        let destructured_filters = self.filters.iter().map(|filter| &filter.name);
        let filter_expressions = self.filters.iter().map(|filter| {
            let request_field = &filter.name;
            let model_field = &filter.field;
            match filter.operation {
                FilterOperation::Between => quote! {
                    if let Some((low, high)) = #request_field {
                        extra_filters.push(#model::fields().#model_field().between(low, high));
                    }
                },
                operation => {
                    let method = operation.method();
                    quote! {
                        if let Some(value) = #request_field {
                            extra_filters.push(#model::fields().#model_field().#method(value));
                        }
                    }
                }
            }
        });
        let default_orders = self.default_order.iter().map(|order| {
            let variant = order_variant(&order.field);
            let direction = order.direction.tokens(&root);
            quote!(#order_type::#variant(#direction),)
        });
        let append_tie_breaker = self.tie_breaker.iter().map(|order| {
            let variant = order_variant(&order.field);
            let direction = order.direction.tokens(&root);
            let field = &order.field;
            quote! {
                if !seen.contains(&stringify!(#field)) {
                    orders.push(#order_type::#variant(#direction));
                }
            }
        });
        let apply_order_arms = self.sorts.iter().flat_map(|field| {
            let variant = order_variant(field);
            let asc = Ident::new("asc", field.span());
            let desc = Ident::new("desc", field.span());
            [
                quote! {
                    #order_type::#variant(#root::query::TcQuerySortDirection::Asc) => {
                        query.order_by(#model::fields().#field().#asc())
                    }
                },
                quote! {
                    #order_type::#variant(#root::query::TcQuerySortDirection::Desc) => {
                        query.order_by(#model::fields().#field().#desc())
                    }
                },
            ]
        });
        let (order_field_impl, apply_orders_method) = if self.sorts.is_empty() {
            (
                TokenStream::new(),
                quote! {
                    fn apply_orders(
                        query: #root::schema::QueryMany<#model>,
                        orders: Vec<#order_type>,
                    ) -> Result<
                        #root::schema::QueryMany<#model>,
                        #root::query::TcQueryBuildError,
                    > {
                        debug_assert!(orders.is_empty());
                        Ok(query)
                    }
                },
            )
        } else {
            (
                quote! {
                    impl #order_type {
                        fn field_name(&self) -> &'static str {
                            match self {
                                #( #order_field_arms )*
                            }
                        }
                    }
                },
                quote! {
                    fn apply_orders(
                        mut query: #root::schema::QueryMany<#model>,
                        mut orders: Vec<#order_type>,
                    ) -> Result<
                        #root::schema::QueryMany<#model>,
                        #root::query::TcQueryBuildError,
                    > {
                        if orders.is_empty() {
                            orders.extend([ #( #default_orders )* ]);
                        }

                        let mut seen = Vec::with_capacity(orders.len() + 1);
                        for order in &orders {
                            let field = order.field_name();
                            if seen.contains(&field) {
                                return Err(
                                    #root::query::TcQueryBuildError::DuplicateSort { field }
                                );
                            }
                            seen.push(field);
                        }
                        #( #append_tie_breaker )*

                        for order in orders {
                            query = match order {
                                #( #apply_order_arms, )*
                            };
                        }
                        Ok(query)
                    }
                },
            )
        };
        let page_impl = self.page.iter().map(|page| {
            let default_size = &page.default_size;
            let max_size = &page.max_size;
            quote! {
                const _: () = {
                    assert!(#default_size > 0, "default_size must be at least 1");
                    assert!(#default_size <= #max_size, "default_size must not exceed max_size");
                };

                impl #name {
                    fn validated_paging(
                        &self,
                    ) -> Result<#root::query::Paging, #root::query::TcQueryBuildError> {
                        if self.page == 0 {
                            return Err(#root::query::TcQueryBuildError::InvalidPageNumber);
                        }
                        if self.size == 0 || self.size > #max_size {
                            return Err(#root::query::TcQueryBuildError::InvalidPageSize {
                                size: self.size,
                                max: #max_size,
                            });
                        }
                        Ok(#root::query::Paging {
                            page: self.page,
                            size: self.size,
                        })
                    }

                    #vis async fn fetch_page(
                        self,
                        executor: &mut dyn #root::Executor,
                    ) -> Result<#root::query::Page<#model>, #root::query::TcQueryError> {
                        let paging = self.validated_paging()?;
                        let size = usize::try_from(paging.size)
                            .map_err(|_| #root::query::TcQueryBuildError::OffsetOverflow)?;
                        let offset = (paging.page - 1)
                            .checked_mul(paging.size)
                            .and_then(|offset| usize::try_from(offset).ok())
                            .ok_or(#root::query::TcQueryBuildError::OffsetOverflow)?;
                        let (expr, orders) = self.into_parts();

                        let total = #model::filter(expr.clone())
                            .count()
                            .exec(executor)
                            .await?;
                        let query = Self::apply_orders(#model::filter(expr), orders)?;
                        let items = query.limit(size).offset(offset).exec(executor).await?;
                        let total_pages = if total == 0 {
                            0
                        } else {
                            (total - 1) / paging.size + 1
                        };

                        Ok(#root::query::Page {
                            items,
                            paging,
                            total,
                            total_pages,
                        })
                    }
                }
            }
        });

        Ok(quote! {
            #[derive(Debug, #root::bon::Builder)]
            #[builder(crate = #root::bon, on(String, into))]
            #vis struct #name {
                #[builder(field = Vec::new())]
                extra_filters: Vec<#root::stmt::Expr<bool>>,
                #[builder(field = Vec::new())]
                orders: Vec<#order_type>,
                #( #filter_fields )*
                #( #page_fields )*
            }

            #[derive(Debug)]
            enum #order_type {
                #( #order_variants )*
            }

            #order_field_impl

            impl Default for #name {
                fn default() -> Self {
                    Self::builder().build()
                }
            }

            impl<S: #builder_module::State> #builder<S> {
                #vis fn filter(mut self, expr: #root::stmt::Expr<bool>) -> Self {
                    self.extra_filters.push(expr);
                    self
                }

                #( #builder_sort_methods )*
            }

            impl #name {
                #vis fn filter(mut self, expr: #root::stmt::Expr<bool>) -> Self {
                    self.extra_filters.push(expr);
                    self
                }

                fn into_parts(self) -> (#root::stmt::Expr<bool>, Vec<#order_type>) {
                    let Self {
                        #( #destructured_filters, )*
                        mut extra_filters,
                        orders,
                        ..
                    } = self;
                    #( #filter_expressions )*
                    (#root::stmt::Expr::and_all(extra_filters), orders)
                }

                #apply_orders_method

                #vis fn into_expr(
                    self,
                ) -> Result<#root::stmt::Expr<bool>, #root::query::TcQueryBuildError> {
                    Ok(self.into_parts().0)
                }

                #vis fn into_query(
                    self,
                ) -> Result<#root::schema::QueryMany<#model>, #root::query::TcQueryBuildError> {
                    let (expr, orders) = self.into_parts();
                    Self::apply_orders(#model::filter(expr), orders)
                }

                #vis async fn count(
                    self,
                    executor: &mut dyn #root::Executor,
                ) -> Result<u64, #root::query::TcQueryError> {
                    let (expr, _) = self.into_parts();
                    Ok(#model::filter(expr).count().exec(executor).await?)
                }

                #vis async fn all(
                    self,
                    executor: &mut dyn #root::Executor,
                ) -> Result<Vec<#model>, #root::query::TcQueryError> {
                    Ok(self.into_query()?.exec(executor).await?)
                }
            }

            #( #page_impl )*
        })
    }
}

impl Direction {
    fn parse(ident: Ident) -> syn::Result<Self> {
        match ident.to_string().as_str() {
            "Asc" => Ok(Self::Asc),
            "Desc" => Ok(Self::Desc),
            _ => Err(syn::Error::new_spanned(ident, "expected Asc or Desc")),
        }
    }

    fn tokens(&self, root: &TokenStream) -> TokenStream {
        match self {
            Self::Asc => quote!(#root::query::TcQuerySortDirection::Asc),
            Self::Desc => quote!(#root::query::TcQuerySortDirection::Desc),
        }
    }
}

impl FilterOperation {
    fn parse(ident: Ident) -> syn::Result<Self> {
        match ident.to_string().as_str() {
            "eq" => Ok(Self::Eq),
            "ne" => Ok(Self::Ne),
            "gt" => Ok(Self::Gt),
            "ge" => Ok(Self::Ge),
            "lt" => Ok(Self::Lt),
            "le" => Ok(Self::Le),
            "between" => Ok(Self::Between),
            "in_list" => Ok(Self::InList),
            "starts_with" => Ok(Self::StartsWith),
            _ => Err(syn::Error::new_spanned(
                ident,
                "unsupported query filter operation",
            )),
        }
    }

    fn method(self) -> Ident {
        Ident::new(
            match self {
                Self::Eq => "eq",
                Self::Ne => "ne",
                Self::Gt => "gt",
                Self::Ge => "ge",
                Self::Lt => "lt",
                Self::Le => "le",
                Self::Between => "between",
                Self::InList => "in_list",
                Self::StartsWith => "starts_with",
            },
            proc_macro2::Span::call_site(),
        )
    }
}

fn parse_keyword(input: ParseStream<'_>, expected: &str) -> syn::Result<()> {
    let actual: Ident = input.parse()?;
    if actual == expected {
        Ok(())
    } else {
        Err(syn::Error::new_spanned(
            actual,
            format!("expected `{expected}`"),
        ))
    }
}

fn next_is_keyword(input: ParseStream<'_>, expected: &str) -> bool {
    input
        .fork()
        .parse::<Ident>()
        .is_ok_and(|ident| ident == expected)
}

fn parse_order(input: ParseStream<'_>) -> syn::Result<Order> {
    let field = input.parse()?;
    let direction = Direction::parse(input.parse()?)?;
    Ok(Order { field, direction })
}

fn order_variant(field: &Ident) -> Ident {
    format_ident!(
        "{}",
        to_upper_camel(&field.to_string()),
        span = field.span()
    )
}

fn to_upper_camel(value: &str) -> String {
    value
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect()
}

fn to_snake_case(value: &str) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    let mut output = String::new();
    for (index, ch) in chars.iter().copied().enumerate() {
        if ch.is_uppercase()
            && index > 0
            && (chars[index - 1].is_lowercase()
                || chars.get(index + 1).is_some_and(|next| next.is_lowercase()))
        {
            output.push('_');
        }
        output.extend(ch.to_lowercase());
    }
    output
}

fn check_unique<'a>(
    values: impl IntoIterator<Item = &'a Ident>,
    message: &str,
    errors: &mut Option<syn::Error>,
) {
    let mut seen = HashSet::new();
    for value in values {
        if !seen.insert(value.to_string()) {
            combine_error(errors, syn::Error::new_spanned(value, message));
        }
    }
}

fn combine_error(errors: &mut Option<syn::Error>, error: syn::Error) {
    if let Some(errors) = errors {
        errors.combine(error);
    } else {
        *errors = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use quote::quote;

    use super::{QuerySpec, to_snake_case};

    #[test]
    fn rejects_unknown_filter_operation() {
        let error = syn::parse2::<QuerySpec>(quote! {
            Search for Customer {
                filters { name: String => name.contains; }
                sort { id; }
                default_order [id Asc];
                tie_breaker id Asc;
            }
        })
        .err()
        .expect("unknown operation should fail");

        assert!(
            error
                .to_string()
                .contains("unsupported query filter operation")
        );
    }

    #[test]
    fn validates_sort_whitelists() {
        let spec = syn::parse2::<QuerySpec>(quote! {
            Search for Customer {
                filters {}
                sort { id; }
                default_order [name Asc];
                tie_breaker created_at Desc;
            }
        })
        .unwrap();
        let error = spec.validate().unwrap_err();
        let message = error.into_compile_error().to_string();

        assert!(message.contains("default_order field must be declared in sort"));
        assert!(message.contains("tie_breaker field must be declared in sort"));
    }

    #[test]
    fn converts_builder_module_names() {
        assert_eq!(to_snake_case("CustomerSearch"), "customer_search");
        assert_eq!(to_snake_case("HTTPQuery"), "http_query");
    }
}
