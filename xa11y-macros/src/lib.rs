use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, Data, DeriveInput, Fields, Lit};

/// Convert a PascalCase identifier to snake_case.
fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            result.push('_');
        }
        result.push(ch.to_ascii_lowercase());
    }
    result
}

/// Convert a PascalCase identifier to SCREAMING_SNAKE_CASE.
fn to_screaming_snake_case(s: &str) -> String {
    to_snake_case(s).to_uppercase()
}

/// Check if a variant/field has `#[py_bind(skip)]`.
fn has_skip(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("py_bind") {
            return false;
        }
        let mut skip = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("skip") {
                skip = true;
            }
            Ok(())
        });
        skip
    })
}

/// Get `#[py_bind(rename = "...")]` value if present.
fn get_rename(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if !attr.path().is_ident("py_bind") {
            continue;
        }
        let mut rename = None;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(s) = lit {
                    rename = Some(s.value());
                }
            }
            Ok(())
        });
        if rename.is_some() {
            return rename;
        }
    }
    None
}

/// Get `#[py_bind(string = "...")]` value if present.
fn get_string_override(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if !attr.path().is_ident("py_bind") {
            continue;
        }
        let mut string_val = None;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("string") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(s) = lit {
                    string_val = Some(s.value());
                }
            }
            Ok(())
        });
        if string_val.is_some() {
            return string_val;
        }
    }
    None
}

/// Check if `#[py_bind(class_attrs)]` is present on the type.
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
/// # Enum behavior
/// Generates a `#[pyclass]` with:
/// - `to_str()` method returning the snake_case string name
/// - `from_str(s)` classmethod accepting the string name
/// - `__repr__` and `__eq__`
///
/// With `#[py_bind(class_attrs)]`, generates `@classattr` constants instead.
///
/// # Struct behavior
/// Generates `#[pyclass(frozen)]` with:
/// - Getter properties for each field
/// - `__repr__` and `__eq__`
/// - `__new__` constructor
///
/// # Attributes
/// - `#[py_bind(skip)]` — exclude a variant/field
/// - `#[py_bind(rename = "...")]` — override Python name
/// - `#[py_bind(string = "...")]` — override string representation for enum variants
/// - `#[py_bind(class_attrs)]` — (enum-level) generate `@classattr` constants
#[proc_macro_derive(PyBindable, attributes(py_bind))]
pub fn derive_py_bindable(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    match &input.data {
        Data::Enum(data_enum) => {
            if has_class_attrs(&input.attrs) {
                generate_class_attrs_enum(name, data_enum)
            } else {
                generate_string_enum(name, data_enum)
            }
        }
        Data::Struct(data_struct) => generate_frozen_struct(name, data_struct),
        Data::Union(_) => syn::Error::new_spanned(&input, "PyBindable does not support unions")
            .to_compile_error()
            .into(),
    }
}

fn generate_string_enum(name: &syn::Ident, data_enum: &syn::DataEnum) -> TokenStream {
    let py_class_name = name.to_string();

    let mut to_str_arms = Vec::new();
    let mut from_str_arms = Vec::new();
    let mut all_strings = Vec::new();
    let mut has_skipped = false;

    for variant in &data_enum.variants {
        if has_skip(&variant.attrs) {
            has_skipped = true;
            continue;
        }

        let variant_ident = &variant.ident;
        let string_name = get_string_override(&variant.attrs)
            .unwrap_or_else(|| to_snake_case(&variant_ident.to_string()));

        to_str_arms.push(quote! {
            #name::#variant_ident => #string_name,
        });

        from_str_arms.push(quote! {
            #string_name => ::std::result::Result::Ok(#name::#variant_ident),
        });

        all_strings.push(string_name);
    }

    let all_strings_joined = all_strings.join(", ");

    let skipped_arm = if has_skipped {
        quote! { _ => panic!("to_str called on skipped variant"), }
    } else {
        quote! {}
    };

    let expanded = quote! {
        impl #name {
            /// Return the snake_case string name for this variant.
            pub fn to_str(&self) -> &'static str {
                match self {
                    #(#to_str_arms)*
                    #skipped_arm
                }
            }
        }

                #[::pyo3::pymethods]
        impl #name {
            /// Return the snake_case string name for this variant (Python-accessible).
            #[pyo3(name = "to_str")]
            fn py_to_str(&self) -> &'static str {
                self.to_str()
            }

            /// Parse a snake_case string into this enum.
            #[classmethod]
            fn from_str(_cls: &::pyo3::Bound<'_, ::pyo3::types::PyType>, s: &str) -> ::pyo3::PyResult<Self> {
                match s {
                    #(#from_str_arms)*
                    _ => ::std::result::Result::Err(
                        ::pyo3::exceptions::PyValueError::new_err(
                            format!("Unknown {} value: '{}'. Valid values: {}", #py_class_name, s, #all_strings_joined)
                        )
                    ),
                }
            }

            fn __repr__(&self) -> String {
                format!("{}.{}", #py_class_name, self.to_str())
            }
        }
    };

    expanded.into()
}

fn generate_class_attrs_enum(name: &syn::Ident, data_enum: &syn::DataEnum) -> TokenStream {
    let py_class_name = name.to_string();

    let mut classattr_consts = Vec::new();
    let mut to_str_arms = Vec::new();

    for variant in &data_enum.variants {
        if has_skip(&variant.attrs) {
            continue;
        }

        let variant_ident = &variant.ident;
        let string_name = get_string_override(&variant.attrs)
            .unwrap_or_else(|| to_snake_case(&variant_ident.to_string()));
        let const_name = get_rename(&variant.attrs)
            .map(|r| format_ident!("{}", r))
            .unwrap_or_else(|| {
                format_ident!("{}", to_screaming_snake_case(&variant_ident.to_string()))
            });

        classattr_consts.push(quote! {
            #[classattr]
            #[allow(non_upper_case_globals)]
            const #const_name: &'static str = #string_name;
        });

        to_str_arms.push(quote! {
            #name::#variant_ident => #string_name,
        });
    }

    // For class_attrs enums, we generate a separate Python class with constants,
    // plus a to_py_str method on the Rust enum for converting instances.
    let py_wrapper = format_ident!("Py{}", name);

    let expanded = quote! {
        /// Python wrapper class exposing enum variants as class-level string constants.
                #[::pyo3::pyclass(frozen, name = #py_class_name)]
        pub struct #py_wrapper;

                #[::pyo3::pymethods]
        impl #py_wrapper {
            #(#classattr_consts)*
        }

                impl #name {
            /// Convert this enum variant to its Python string representation.
            pub fn to_py_str(&self) -> &'static str {
                match self {
                    #(#to_str_arms)*
                }
            }
        }
    };

    expanded.into()
}

fn generate_frozen_struct(name: &syn::Ident, data_struct: &syn::DataStruct) -> TokenStream {
    let py_class_name = name.to_string();

    let fields = match &data_struct.fields {
        Fields::Named(named) => &named.named,
        _ => {
            return syn::Error::new_spanned(
                name,
                "PyBindable only supports structs with named fields",
            )
            .to_compile_error()
            .into();
        }
    };

    let mut field_infos = Vec::new();
    let mut skipped_fields = Vec::new();
    for field in fields {
        let field_ident = field.ident.as_ref().unwrap();
        if has_skip(&field.attrs) {
            skipped_fields.push(field_ident.clone());
            continue;
        }
        let py_name = get_rename(&field.attrs).unwrap_or_else(|| field_ident.to_string());
        let field_ty = &field.ty;
        field_infos.push((field_ident.clone(), py_name, field_ty.clone()));
    }

    let getter_methods: Vec<_> = field_infos
        .iter()
        .map(|(ident, py_name, ty)| {
            let getter_name = format_ident!("{}", py_name);
            quote! {
                #[getter]
                fn #getter_name(&self) -> #ty {
                    self.#ident
                }
            }
        })
        .collect();

    let new_params: Vec<_> = field_infos
        .iter()
        .map(|(ident, _py_name, ty)| {
            quote! { #ident: #ty }
        })
        .collect();

    let new_fields: Vec<_> = field_infos
        .iter()
        .map(|(ident, _, _)| {
            quote! { #ident }
        })
        .collect();

    let skipped_field_defaults: Vec<_> = skipped_fields
        .iter()
        .map(|ident| {
            quote! { #ident: ::std::default::Default::default() }
        })
        .collect();

    // Build the format string and args separately
    let fmt_str = {
        let mut s = format!("{}(", py_class_name);
        for (i, (_ident, py_name, _)) in field_infos.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&format!("{}={{}}", py_name));
        }
        s.push(')');
        s
    };

    let fmt_args: Vec<_> = field_infos
        .iter()
        .map(|(ident, _, _)| {
            quote! { self.#ident }
        })
        .collect();

    let expanded = quote! {
        #[::pyo3::pymethods]
        impl #name {
            #[new]
            fn __new__(#(#new_params),*) -> Self {
                Self { #(#new_fields,)* #(#skipped_field_defaults,)* }
            }

            #(#getter_methods)*

            fn __repr__(&self) -> String {
                format!(#fmt_str, #(#fmt_args),*)
            }
        }
    };

    expanded.into()
}
