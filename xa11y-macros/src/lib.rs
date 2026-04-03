use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, Data, DeriveInput, Fields};

fn to_snake_case(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

fn has_class_attrs(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("py_bind") {
            return false;
        }
        let mut found = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("class_attrs") {
                found = true;
            }
            Ok(())
        });
        found
    })
}

/// Derive macro that generates PyO3 Python bindings for enums and structs.
///
/// - Enums get `to_str()`, `from_str()`, and `__repr__`.
/// - Enums with `#[py_bind(class_attrs)]` get a `PyFoo` wrapper with `@classattr` constants.
/// - Structs get `__new__`, getters, and `__repr__`.
#[proc_macro_derive(PyBindable, attributes(py_bind))]
pub fn derive_py_bindable(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    match &input.data {
        Data::Enum(data) if has_class_attrs(&input.attrs) => class_attrs_enum(name, data),
        Data::Enum(data) => string_enum(name, data),
        Data::Struct(data) => frozen_struct(name, data),
        Data::Union(_) => syn::Error::new_spanned(&input, "unions not supported")
            .to_compile_error()
            .into(),
    }
}

fn string_enum(name: &syn::Ident, data: &syn::DataEnum) -> TokenStream {
    let cls = name.to_string();
    let variants: Vec<_> = data.variants.iter().collect();
    let snake: Vec<String> = variants
        .iter()
        .map(|v| to_snake_case(&v.ident.to_string()))
        .collect();
    let idents: Vec<&syn::Ident> = variants.iter().map(|v| &v.ident).collect();
    let all = snake.join(", ");

    quote! {
        impl #name {
            pub fn to_str(&self) -> &'static str {
                match self { #( #name::#idents => #snake, )* }
            }
        }

        #[::pyo3::pymethods]
        impl #name {
            #[pyo3(name = "to_str")]
            fn _to_str(&self) -> &'static str { self.to_str() }

            #[classmethod]
            fn from_str(_cls: &::pyo3::Bound<'_, ::pyo3::types::PyType>, s: &str) -> ::pyo3::PyResult<Self> {
                match s {
                    #( #snake => Ok(#name::#idents), )*
                    _ => Err(::pyo3::exceptions::PyValueError::new_err(
                        format!("Unknown {} '{}'. Valid: {}", #cls, s, #all)
                    )),
                }
            }

            fn __repr__(&self) -> String { format!("{}.{}", #cls, self.to_str()) }
        }
    }
    .into()
}

fn class_attrs_enum(name: &syn::Ident, data: &syn::DataEnum) -> TokenStream {
    let cls = name.to_string();
    let wrapper = format_ident!("Py{}", name);
    let variants: Vec<_> = data.variants.iter().collect();
    let snake: Vec<String> = variants
        .iter()
        .map(|v| to_snake_case(&v.ident.to_string()))
        .collect();
    let idents: Vec<&syn::Ident> = variants.iter().map(|v| &v.ident).collect();
    let const_names: Vec<syn::Ident> = snake
        .iter()
        .map(|s| format_ident!("{}", s.to_uppercase()))
        .collect();

    quote! {
        #[::pyo3::pyclass(frozen, name = #cls)]
        pub struct #wrapper;

        #[::pyo3::pymethods]
        impl #wrapper {
            #( #[classattr] const #const_names: &'static str = #snake; )*
        }

        impl #name {
            pub fn to_py_str(&self) -> &'static str {
                match self { #( #name::#idents => #snake, )* }
            }
        }
    }
    .into()
}

fn frozen_struct(name: &syn::Ident, data: &syn::DataStruct) -> TokenStream {
    let cls = name.to_string();
    let fields = match &data.fields {
        Fields::Named(f) => &f.named,
        _ => {
            return syn::Error::new_spanned(name, "only named fields supported")
                .to_compile_error()
                .into()
        }
    };

    let idents: Vec<_> = fields.iter().map(|f| f.ident.as_ref().unwrap()).collect();
    let types: Vec<_> = fields.iter().map(|f| &f.ty).collect();
    let names: Vec<String> = idents.iter().map(|i| i.to_string()).collect();

    let fmt = format!(
        "{}({})",
        cls,
        names
            .iter()
            .map(|n| format!("{}={{}}", n))
            .collect::<Vec<_>>()
            .join(", ")
    );

    quote! {
        #[::pyo3::pymethods]
        impl #name {
            #[new]
            fn __new__(#( #idents: #types ),*) -> Self { Self { #( #idents ),* } }

            #( #[getter] fn #idents(&self) -> #types { self.#idents } )*

            fn __repr__(&self) -> String { format!(#fmt, #( self.#idents ),*) }
        }
    }
    .into()
}
