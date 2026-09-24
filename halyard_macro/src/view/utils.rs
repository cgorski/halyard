use proc_macro2::Ident;
use quote::format_ident;
use rstml::node::{CustomNode, KeyedAttribute, NodeElement, NodeName};
use syn::{spanned::Spanned, ExprPath};

/// Converts a simple literal (string, char, integer or float) to its string representation.
///
/// A literal wrapped in a block, like `{"string"}`, is not converted.
pub fn value_to_string(value: &syn::Expr) -> Option<String> {
    match &value {
        syn::Expr::Lit(lit) => match &lit.lit {
            syn::Lit::Str(s) => Some(s.value()),
            syn::Lit::Char(c) => Some(c.value().to_string()),
            syn::Lit::Int(i) => Some(i.base10_digits().to_string()),
            syn::Lit::Float(f) => Some(f.base10_digits().to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// Whether an element names a component: its tag is a path whose last segment starts with
/// an ASCII uppercase letter (`<Foo/>`, `<module::Foo/>`).
pub fn is_component_node(node: &NodeElement<impl CustomNode>) -> bool {
    match node.name() {
        NodeName::Path(path) => {
            path.path.segments.last().is_some_and(|segment| {
                segment
                    .ident
                    .to_string()
                    .starts_with(|c: char| c.is_ascii_uppercase())
            })
        }
        NodeName::Block(_) | NodeName::Punctuated(_) => false,
    }
}

pub fn filter_prefixed_attrs<'a, A>(attrs: A, prefix: &str) -> Vec<Ident>
where
    A: IntoIterator<Item = &'a KeyedAttribute> + Clone,
{
    attrs
        .into_iter()
        .filter_map(|attr| {
            attr.key
                .to_string()
                .strip_prefix(prefix)
                .map(|ident| format_ident!("{ident}", span = attr.key.span()))
        })
        .collect()
}

/// Handle nostrip: prefix:
/// if there strip from the name, and return true to indicate that
/// the prop should be an Option<T> and shouldn't be called on the builder if None,
/// if Some(T) then T supplied to the builder.
pub fn is_nostrip_optional_and_update_key(key: &mut NodeName) -> bool {
    let maybe_cleaned_name_and_span = if let NodeName::Punctuated(punct) = &key
    {
        if punct.len() == 2 {
            if let Some(cleaned_name) = key.to_string().strip_prefix("nostrip:")
            {
                punct
                    .get(1)
                    .map(|segment| (cleaned_name.to_string(), segment.span()))
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    if let Some((cleaned_name, span)) = maybe_cleaned_name_and_span {
        *key = NodeName::Path(ExprPath {
            attrs: vec![],
            qself: None,
            path: format_ident!("{}", cleaned_name, span = span).into(),
        });
        true
    } else {
        false
    }
}
