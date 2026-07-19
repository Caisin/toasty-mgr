use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::{
    Attribute, Data, DeriveInput, Field, Fields, GenericArgument, Ident, LitStr, PathArguments,
    Type,
};

pub(crate) fn expand(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            input.generics,
            "TcQuery does not support generic models",
        ));
    }

    let fields = match input.data {
        Data::Struct(data) => match data.fields {
            Fields::Named(fields) => fields.named,
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "TcQuery requires a struct with named fields",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new(
                input.ident.span(),
                "TcQuery can only be derived for structs",
            ));
        }
    };

    let crate_path = runtime_crate_path()?;
    let model = input.ident;
    let vis = input.vis;
    let extension = format_ident!("{}TcQueryExt", model);

    let mut errors: Option<syn::Error> = None;
    let mut trait_methods = Vec::new();
    let mut query_methods = Vec::new();

    for field in fields {
        match expand_field(&crate_path, &model, &field) {
            Ok(Some((signatures, methods))) => {
                trait_methods.extend(signatures);
                query_methods.extend(methods);
            }
            Ok(None) => {}
            Err(error) => combine_error(&mut errors, error),
        }
    }

    if let Some(error) = errors {
        return Err(error);
    }

    Ok(quote! {
        #vis trait #extension: Sized {
            #( #trait_methods )*
        }

        impl #extension for #crate_path::schema::QueryMany<#model> {
            #( #query_methods )*
        }
    })
}

fn expand_field(
    crate_path: &proc_macro2::TokenStream,
    model: &Ident,
    field: &Field,
) -> syn::Result<Option<(Vec<proc_macro2::TokenStream>, Vec<proc_macro2::TokenStream>)>> {
    let field_ident = field
        .ident
        .as_ref()
        .expect("named fields always have identifiers");

    if is_relation(&field.attrs) {
        if let Some(attr) = field
            .attrs
            .iter()
            .find(|attr| attr.path().is_ident("tc_query"))
        {
            return Err(syn::Error::new_spanned(
                attr,
                "TcQuery attributes are not allowed on Toasty relationship fields",
            ));
        }
        return Ok(None);
    }

    let config = FieldConfig::parse(&field.attrs)?;
    if config.skip.is_some() {
        return Ok(None);
    }

    let classification = classify_type(&field.ty);
    let operations = match (config.operations, classification) {
        (Some(operations), _) => operations,
        (None, Some(classification)) => classification.operations(),
        (None, None) => {
            return Err(syn::Error::new_spanned(
                &field.ty,
                format!(
                    "cannot infer TcQuery operations for database field `{field_ident}`; use #[tc_query(ops(...), sort)] or #[tc_query(skip = \"reason\")]"
                ),
            ));
        }
    };
    let sortable = config
        .sort
        .unwrap_or_else(|| classification.is_some_and(FieldClassification::sortable_by_default));

    let mut trait_methods = Vec::new();
    let mut query_methods = Vec::new();
    for operation in operations {
        let (signature, method) =
            expand_operation(crate_path, model, field_ident, &field.ty, operation);
        trait_methods.push(signature);
        query_methods.push(method);
    }

    if sortable {
        let (signature, method) = expand_sort(model, field_ident, SortDirection::Asc);
        trait_methods.push(signature);
        query_methods.push(method);
        let (signature, method) = expand_sort(model, field_ident, SortDirection::Desc);
        trait_methods.push(signature);
        query_methods.push(method);
    }

    Ok(Some((trait_methods, query_methods)))
}

fn expand_operation(
    crate_path: &proc_macro2::TokenStream,
    model: &Ident,
    field: &Ident,
    field_ty: &Type,
    operation: Operation,
) -> (proc_macro2::TokenStream, proc_macro2::TokenStream) {
    let suffix = operation.name();
    let method = format_ident!("{}_{}", clean_ident(field), suffix, span = field.span());
    let operation_ident = Ident::new(suffix, Span::call_site());

    match operation {
        Operation::Between => (
            quote! {
                fn #method<__L, __H>(self, low: __L, high: __H) -> Self
                where
                    __L: #crate_path::stmt::IntoExpr<#field_ty>,
                    __H: #crate_path::stmt::IntoExpr<#field_ty>;
            },
            quote! {
                fn #method<__L, __H>(self, low: __L, high: __H) -> Self
                where
                    __L: #crate_path::stmt::IntoExpr<#field_ty>,
                    __H: #crate_path::stmt::IntoExpr<#field_ty>,
                {
                    self.filter(#model::fields().#field().between(low, high))
                }
            },
        ),
        Operation::InList => (
            quote! {
                fn #method<__V>(self, values: __V) -> Self
                where
                    __V: #crate_path::stmt::IntoExpr<#crate_path::stmt::List<#field_ty>>;
            },
            quote! {
                fn #method<__V>(self, values: __V) -> Self
                where
                    __V: #crate_path::stmt::IntoExpr<#crate_path::stmt::List<#field_ty>>,
                {
                    self.filter(#model::fields().#field().in_list(values))
                }
            },
        ),
        Operation::StartsWith => (
            quote! {
                fn #method<__V>(self, value: __V) -> Self
                where
                    __V: #crate_path::stmt::IntoExpr<String>;
            },
            quote! {
                fn #method<__V>(self, value: __V) -> Self
                where
                    __V: #crate_path::stmt::IntoExpr<String>,
                {
                    self.filter(#model::fields().#field().starts_with(value))
                }
            },
        ),
        Operation::IsNone | Operation::IsSome => (
            quote!(fn #method(self) -> Self;),
            quote! {
                fn #method(self) -> Self {
                    self.filter(#model::fields().#field().#operation_ident())
                }
            },
        ),
        Operation::Eq
        | Operation::Ne
        | Operation::Gt
        | Operation::Ge
        | Operation::Lt
        | Operation::Le => (
            quote! {
                fn #method<__V>(self, value: __V) -> Self
                where
                    __V: #crate_path::stmt::IntoExpr<#field_ty>;
            },
            quote! {
                fn #method<__V>(self, value: __V) -> Self
                where
                    __V: #crate_path::stmt::IntoExpr<#field_ty>,
                {
                    self.filter(#model::fields().#field().#operation_ident(value))
                }
            },
        ),
    }
}

fn expand_sort(
    model: &Ident,
    field: &Ident,
    direction: SortDirection,
) -> (proc_macro2::TokenStream, proc_macro2::TokenStream) {
    let direction_name = direction.name();
    let method = format_ident!(
        "{}_{}",
        direction_name,
        clean_ident(field),
        span = field.span()
    );
    let direction_ident = Ident::new(direction_name, Span::call_site());
    (
        quote!(fn #method(self) -> Self;),
        quote! {
            fn #method(self) -> Self {
                self.order_by(#model::fields().#field().#direction_ident())
            }
        },
    )
}

pub(crate) fn runtime_crate_path() -> syn::Result<proc_macro2::TokenStream> {
    match crate_name("toasty-mgr") {
        Ok(FoundCrate::Itself) => Ok(quote!(::toasty_mgr)),
        Ok(FoundCrate::Name(name)) => {
            let ident = Ident::new(&name, Span::call_site());
            Ok(quote!(::#ident))
        }
        Err(error) => Err(syn::Error::new(
            Span::call_site(),
            format!("failed to locate toasty-mgr: {error}"),
        )),
    }
}

fn is_relation(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        ["belongs_to", "has_one", "has_many"]
            .iter()
            .any(|name| attr.path().is_ident(name))
    })
}

fn clean_ident(ident: &Ident) -> String {
    ident.to_string().trim_start_matches("r#").to_owned()
}

fn combine_error(errors: &mut Option<syn::Error>, error: syn::Error) {
    if let Some(errors) = errors {
        errors.combine(error);
    } else {
        *errors = Some(error);
    }
}

#[derive(Clone, Copy)]
enum FieldClassification {
    Scalar,
    Ordered,
    String,
    OptionalScalar,
    OptionalOrdered,
    OptionalString,
}

impl FieldClassification {
    fn operations(self) -> Vec<Operation> {
        let mut operations = vec![Operation::Eq, Operation::Ne, Operation::InList];
        if matches!(
            self,
            Self::Ordered | Self::String | Self::OptionalOrdered | Self::OptionalString
        ) {
            operations.extend([
                Operation::Gt,
                Operation::Ge,
                Operation::Lt,
                Operation::Le,
                Operation::Between,
            ]);
        }
        if matches!(self, Self::String | Self::OptionalString) {
            operations.push(Operation::StartsWith);
        }
        if matches!(
            self,
            Self::OptionalScalar | Self::OptionalOrdered | Self::OptionalString
        ) {
            operations.extend([Operation::IsNone, Operation::IsSome]);
        }
        operations
    }

    fn sortable_by_default(self) -> bool {
        matches!(
            self,
            Self::Ordered | Self::String | Self::OptionalOrdered | Self::OptionalString
        )
    }
}

fn classify_type(ty: &Type) -> Option<FieldClassification> {
    let Type::Path(path) = ty else {
        return None;
    };
    let segment = path.path.segments.last()?;
    let ident = segment.ident.to_string();

    if ident == "Option" {
        let inner = first_type_argument(&segment.arguments)?;
        return match classify_type(inner)? {
            FieldClassification::Scalar | FieldClassification::OptionalScalar => {
                Some(FieldClassification::OptionalScalar)
            }
            FieldClassification::Ordered | FieldClassification::OptionalOrdered => {
                Some(FieldClassification::OptionalOrdered)
            }
            FieldClassification::String | FieldClassification::OptionalString => {
                Some(FieldClassification::OptionalString)
            }
        };
    }

    if ident == "String" {
        return Some(FieldClassification::String);
    }
    if ident == "bool" || ident == "Uuid" || is_byte_vec(segment) {
        return Some(FieldClassification::Scalar);
    }
    if is_ordered_type(&ident) {
        return Some(FieldClassification::Ordered);
    }
    None
}

fn first_type_argument(arguments: &PathArguments) -> Option<&Type> {
    let PathArguments::AngleBracketed(arguments) = arguments else {
        return None;
    };
    arguments.args.iter().find_map(|argument| match argument {
        GenericArgument::Type(ty) => Some(ty),
        _ => None,
    })
}

fn is_byte_vec(segment: &syn::PathSegment) -> bool {
    if segment.ident != "Vec" {
        return false;
    }
    first_type_argument(&segment.arguments)
        .is_some_and(|ty| matches!(ty, Type::Path(path) if path.path.is_ident("u8")))
}

fn is_ordered_type(ident: &str) -> bool {
    matches!(
        ident,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
            | "Decimal"
            | "BigDecimal"
            | "Timestamp"
            | "Zoned"
            | "Date"
            | "Time"
            | "DateTime"
    )
}

struct FieldConfig {
    operations: Option<Vec<Operation>>,
    sort: Option<bool>,
    skip: Option<LitStr>,
}

impl FieldConfig {
    fn parse(attrs: &[Attribute]) -> syn::Result<Self> {
        let mut config = Self {
            operations: None,
            sort: None,
            skip: None,
        };

        for attr in attrs.iter().filter(|attr| attr.path().is_ident("tc_query")) {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("ops") {
                    if config.operations.is_some() {
                        return Err(meta.error("duplicate ops configuration"));
                    }
                    let mut operations = Vec::new();
                    meta.parse_nested_meta(|operation| {
                        let ident = operation
                            .path
                            .get_ident()
                            .ok_or_else(|| operation.error("operation must be an identifier"))?;
                        let parsed = Operation::parse(ident)?;
                        if operations.contains(&parsed) {
                            return Err(operation.error("duplicate TcQuery operation"));
                        }
                        operations.push(parsed);
                        Ok(())
                    })?;
                    if operations.is_empty() {
                        return Err(meta.error("ops requires at least one operation"));
                    }
                    config.operations = Some(operations);
                    return Ok(());
                }
                if meta.path.is_ident("sort") {
                    if config.sort.replace(true).is_some() {
                        return Err(meta.error("duplicate sort configuration"));
                    }
                    return Ok(());
                }
                if meta.path.is_ident("skip") {
                    if config.skip.is_some() {
                        return Err(meta.error("duplicate skip configuration"));
                    }
                    let value = meta.value()?.parse::<LitStr>()?;
                    if value.value().trim().is_empty() {
                        return Err(meta.error("skip requires a non-empty reason"));
                    }
                    config.skip = Some(value);
                    return Ok(());
                }
                Err(meta.error("expected ops(...), sort, or skip = \"reason\""))
            })?;
        }

        if let Some(skip) = &config.skip
            && (config.operations.is_some() || config.sort.is_some())
        {
            return Err(syn::Error::new(
                skip.span(),
                "skip cannot be combined with ops or sort",
            ));
        }
        Ok(config)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Between,
    InList,
    StartsWith,
    IsNone,
    IsSome,
}

impl Operation {
    fn parse(ident: &Ident) -> syn::Result<Self> {
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
            "is_none" => Ok(Self::IsNone),
            "is_some" => Ok(Self::IsSome),
            _ => Err(syn::Error::new_spanned(
                ident,
                "unsupported TcQuery operation",
            )),
        }
    }

    fn name(self) -> &'static str {
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
            Self::IsNone => "is_none",
            Self::IsSome => "is_some",
        }
    }
}

enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    fn name(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

#[cfg(test)]
mod tests {
    use syn::{Field, parse_quote};

    use super::{FieldClassification, FieldConfig, classify_type, is_relation};

    #[test]
    fn classifies_supported_database_fields() {
        assert!(matches!(
            classify_type(&parse_quote!(String)),
            Some(FieldClassification::String)
        ));
        assert!(matches!(
            classify_type(&parse_quote!(Option<i64>)),
            Some(FieldClassification::OptionalOrdered)
        ));
        assert!(matches!(
            classify_type(&parse_quote!(Vec<u8>)),
            Some(FieldClassification::Scalar)
        ));
        assert!(classify_type(&parse_quote!(CustomId)).is_none());
    }

    #[test]
    fn recognizes_relationship_attributes_without_type_guessing() {
        let field: Field = parse_quote! {
            #[belongs_to(key = owner_id, references = id)]
            owner: toasty::Deferred<Owner>
        };
        assert!(is_relation(&field.attrs));
    }

    #[test]
    fn requires_a_reason_when_skipping_database_fields() {
        let field: Field = parse_quote! {
            #[tc_query(skip = "")]
            metadata: CustomMetadata
        };
        let error = FieldConfig::parse(&field.attrs).err().unwrap();
        assert!(error.to_string().contains("non-empty reason"));
    }
}
