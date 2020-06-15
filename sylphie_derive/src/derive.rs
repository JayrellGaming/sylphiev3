use darling::*;
use git2::{*, Error as GitError};
use proc_macro::TokenStream;
use proc_macro2::{TokenStream as SynTokenStream};
use static_events_internals::{*, Result};
use static_events_internals::utils::*;
use syn::*;
use syn::spanned::Spanned;
use quote::*;

#[derive(Default)]
struct FieldAttrs {
    is_module_info: bool,
    is_submodule: bool,
    is_core_ref: bool,
    init_with: Option<Expr>,
}
impl FieldAttrs {
    fn from_attrs(attrs: &[Attribute]) -> Result<FieldAttrs> {
        let mut tp = FieldAttrs::default();
        let mut exclusive_count = 0;
        let mut attr_span = None;
        for attr in attrs {
            let mut set_span = true;
            match last_path_segment(&attr.path).as_str() {
                "module_info" if !tp.is_module_info => {
                    tp.is_module_info = true;
                    exclusive_count += 1;
                },
                "submodule" if !tp.is_submodule => {
                    tp.is_submodule = true;
                    exclusive_count += 1;
                },
                "core_ref" if !tp.is_core_ref => {
                    tp.is_core_ref = true;
                    exclusive_count += 1;
                },
                "init_with" => {
                    if tp.init_with.is_some() {
                        error(attr.span(), "Only one #[init_with] attribute can be used.")?;
                    }
                    let expr = attr.parse_args::<Expr>()?;
                    tp.init_with = Some(expr);
                    exclusive_count += 1;
                }
                _ => set_span = false,
            }
            if set_span {
                attr_span = Some(attr.span());
            }
        }
        if exclusive_count > 1 {
            error(
                attr_span.unwrap(),
                "Only one of #[init_with], #[module_info], #[submodule], or #[core_ref] may be \
                 used on one field.",
            )?;
        }
        Ok(tp)
    }
}

#[derive(FromDeriveInput)]
#[darling(attributes(module))]
struct ModuleAttrs {
    #[darling(default)]
    integral: bool,
    #[darling(default)]
    integral_recursive: bool,
    #[darling(default)]
    anonymous: bool,
    #[darling(default)]
    __sylphie_self_crate: bool,
}

fn git_metadata(core: &SynTokenStream) -> std::result::Result<SynTokenStream, GitError> {
    let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
        Ok(v) => v,
        _ => return Err(GitError::from_str("env error")),
    };
    let repo: Repository = Repository::discover(manifest_dir)?;

    let head = repo.head()?;

    let revision = head.peel_to_commit()?.id().to_string();
    let name = head.shorthand().unwrap_or(&revision);
    let changed_files = repo.diff_tree_to_workdir(Some(&head.peel_to_tree()?), None)?.deltas()
        .filter(|x| x.status() != Delta::Unmodified)
        .count() as u32;

    Ok(quote! {
        #core::module::GitInfo {
            name: #name,
            revision: #revision,
            modified_files: #changed_files,
        }
    })
}
fn module_metadata(core: &SynTokenStream, attrs: &ModuleAttrs) -> SynTokenStream {
    let mut flags = SynTokenStream::new();
    if attrs.integral {
        flags.extend(quote! { | #core::module::ModuleFlag::Integral });
    }
    if attrs.integral_recursive {
        flags.extend(quote! { | #core::module::ModuleFlag::IntegralRecursive });
    }
    if attrs.anonymous {
        flags.extend(quote! { | #core::module::ModuleFlag::Anonymous });
    }
    let git_info = match git_metadata(core) {
        Ok(v) => quote! { #core::__macro_export::Some(#v) },
        _ => quote! { #core::__macro_export::None },
    };
    quote! {
        #core::module::ModuleMetadata {
            module_path: ::std::module_path!(),
            crate_version: ::std::option_env!("CARGO_PKG_VERSION").unwrap_or("<unknown>"),
            git_info: #git_info,
            flags: #core::__macro_export::EnumSet::new() #flags,
        }
    }
}
fn derive_module(
    core: &SynTokenStream, input: &mut DeriveInput, attrs: &ModuleAttrs,
) -> Result<SynTokenStream> {
    let input_span = input.span();
    let data = if let Data::Struct(data) = &mut input.data {
        data
    } else {
        error(input.span(), "#[derive(Module)] may only be used with structs.")?
    };
    if let Fields::Named(_) = data.fields {
        // ...
    } else {
        error(input_span, "#[derive(Module)] can only be used on structs with named fields.")?;
    }

    let metadata = module_metadata(core, &attrs);

    let ident = &input.ident;
    let impl_generics = &input.generics;
    let (bounds, ty_bounds, where_bounds) = impl_generics.split_for_impl();

    let mut field_names = Vec::new();
    let mut fields = Vec::new();
    let mut info_field = None;
    for field in &mut data.fields {
        let attrs = FieldAttrs::from_attrs(&field.attrs)?;

        if attrs.is_module_info {
            if info_field.is_some() {
                error(field.span(), "Only one #[module_info] field may be present.")?;
            }
            info_field = Some(&field.ident);
        }

        field_names.push(field.ident.clone().unwrap());
        if let Some(init_with) = attrs.init_with {
            fields.push(quote! { #init_with });
        } else if attrs.is_submodule {
            // Push a `#[subhandler]` attribute to pass to static-events
            field.attrs.push(Attribute {
                pound_token: Default::default(),
                style: AttrStyle::Outer,
                bracket_token: Default::default(),
                path: parse2(quote!(subhandler))?,
                tokens: Default::default(),
            });

            let name = &field.ident;
            fields.push(quote! {
                __mod_walker.register_module(__mod_core, __mod_parent, stringify!(#name))
            });
        } else if attrs.is_core_ref {
            fields.push(quote! { #core::__macro_priv::cast_core_ref(__mod_core) });
        } else {
            fields.push(quote! { #core::__macro_export::Default::default() });
        }
    }
    let info_field = match info_field {
        Some(v) => v,
        _ => error(input_span, "At least one field must be marked with #[module_info].")?,
    };

    Ok(quote! {
        impl #bounds #core::module::Module for #ident #ty_bounds #where_bounds {
            fn metadata(&self) -> #core::module::ModuleMetadata {
                #metadata
            }

            fn info(&self) -> &#core::module::ModuleInfo {
                &self.#info_field
            }
            fn info_mut(&mut self) -> &mut #core::module::ModuleInfo {
                &mut self.#info_field
            }

            fn init_module<R: #core::module::Module>(
                __mod_core: #core::core::CoreRef<R>,
                __mod_parent: &str,
                __mod_walker: &mut #core::module::ModuleTreeWalker,
            ) -> Self {
                #ident {
                    #(#field_names: #fields,)*
                }
            }
        }
    })
}

pub fn derive_events(input: TokenStream) -> Result<TokenStream> {
    let mut input: DeriveInput = parse(input)?;
    let attrs: ModuleAttrs = ModuleAttrs::from_derive_input(&input)?;

    let core = if attrs.__sylphie_self_crate {
        quote! { crate }
    } else {
        quote! { ::sylphie_core }
    };
    let module_impl = match derive_module(&core, &mut input, &attrs) {
        Ok(v) => v,
        Err(e) => e.emit().into(),
    };
    let mut events = DeriveStaticEvents::new(
        &input, Some(quote! { #core::__macro_export::static_events }),
    )?;
    events.add_discriminator(parse2(quote! { #core::__macro_priv::ModuleImplPhase })?);
    let events_impl = events.generate();

    Ok((quote! {
        const _: () = {
            #module_impl
            #events_impl
        };
    }).into())
}