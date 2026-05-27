use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, parse_macro_input};

/// Derive macro that implements [`TryFrom<TypeMap>`] for `T`.
///
/// Apply this to your state struct and each named field will be extracted from the
/// [`TypeMap`] by type automatically. Keep in mind each field must be of a
/// different type — if you need to store multiple values of the same type, use
/// a wrapper like `struct Foo(type)` or `enum Foo { A(type), B(type) }`.
///
/// How the state is stored and cloned is up to you —
/// if you want cheap clones, wrap your fields in `Arc` individually or use
/// `Arc<AppState>` as your axum state type.
///
/// ```ignore
/// #[derive(AppState)]
/// struct AppState {
///     config: Config,
///     pool: Pool,
///     db: Db,
/// }
/// // To wrap the whole state in `Arc`, implement `TryFrom<TypeMap>` for `Arc<AppState>`:
/// impl TryFrom<TypeMap> for Arc<AppState> {
///     type Error = axum_app_wrapper::Error;
///
///     fn try_from(map: TypeMap) -> Result<Self, Self::Error> {
///         Ok(Arc::new(AppState::try_from(map)?))
///     }
/// }
/// ```
#[proc_macro_derive(AppState)]
pub fn derive_app_state(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(f) => &f.named,
            _ => {
                return syn::Error::new_spanned(
                    name,
                    "#[derive(AppState)] only supports structs with named fields",
                )
                .to_compile_error()
                .into();
            }
        },
        _ => {
            return syn::Error::new_spanned(
                name,
                "#[derive(AppState)] can only be applied to structs",
            )
            .to_compile_error()
            .into();
        }
    };

    let field_names = fields.iter().map(|f| &f.ident);
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    quote! {
        impl #impl_generics ::std::convert::TryFrom<::axum_app_wrapper::TypeMap> for #name #ty_generics #where_clause {
            type Error = ::axum_app_wrapper::Error;

            fn try_from(mut map: ::axum_app_wrapper::TypeMap) -> ::std::result::Result<Self, Self::Error> {
                Ok(#name {
                    #(#field_names: ::axum_app_wrapper::extract_type_field(&mut map)?,)*
                })
            }
        }
    }
    .into()
}
