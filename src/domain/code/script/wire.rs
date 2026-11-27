//! The persisted script evidence schema. Tag values and field order are stable.
use super::{Binding, Evidence, Forward, Import, Lookup, Scope};
use crate::domain::code::cache::{record, tagged};

record!(Evidence {
    scopes,
    imports,
    uses,
    exports,
    forward,
    stars,
    packages,
    wildcards,
    all,
    dynamic_all,
    families,
    shapes,
    result_shapes,
    annotated_returns,
    annotated_shapes,
    result_types,
    element_types,
    iterated,
    props,
    members,
    types,
    functions,
    destructured_parameters,
    self_params
});

record!(Scope {
    shape,
    receiver,
    this_boundary,
    parent,
    owner,
    bindings,
    directives
});

record!(Import { module, name });

tagged!(Binding {
    0 => Local(id),
    1 => Import(id),
    2 => Package,
    3 => Conflicting,
    4 => Namespace { import, local },
});

tagged!(Forward {
    0 => Namespace(id),
    1 => Local(name),
    2 => Import(id),
});

tagged!(Lookup {
    0 => Name { scope, receiver },
    1 => Argument { scope, callee },
    2 => Known(id),
    3 => Chained { prior, scope, qualification },
    4 => Construct { scope },
    5 => Prop { scope, component },
    6 => Imported(id),
    7 => Super { scope, base_scope, base },
    8 => PropOf(prior),
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::code::cache::Wire;

    #[test]
    fn lookup_payloads_keep_their_published_tags_and_field_order() {
        let cases: [(Lookup, &[u8]); 9] = [
            (
                Lookup::Name {
                    scope: 300,
                    receiver: Some(2..5),
                },
                &[0, 172, 2, 1, 2, 5],
            ),
            (
                Lookup::Argument {
                    scope: 6,
                    callee: 2..8,
                },
                &[1, 6, 2, 8],
            ),
            (Lookup::Known(4), &[2, 4]),
            (
                Lookup::Chained {
                    prior: 7,
                    scope: 9,
                    qualification: None,
                },
                &[3, 7, 9, 0],
            ),
            (Lookup::Construct { scope: 11 }, &[4, 11]),
            (
                Lookup::Prop {
                    scope: 3,
                    component: 8..13,
                },
                &[5, 3, 8, 13],
            ),
            (Lookup::PropOf(300), &[8, 172, 2]),
            (Lookup::Imported(2), &[6, 2]),
            (
                Lookup::Super {
                    scope: 2,
                    base_scope: 8,
                    base: 4..10,
                },
                &[7, 2, 8, 4, 10],
            ),
        ];
        for (lookup, bytes) in cases {
            let mut encoded = Vec::new();
            lookup.save(&mut encoded);
            assert_eq!(encoded, bytes);
            let mut source = bytes;
            let decoded = Lookup::load(&mut source).unwrap();
            assert!(source.is_empty());
            encoded.clear();
            decoded.save(&mut encoded);
            assert_eq!(encoded, bytes);
            assert!(Lookup::load(&mut &bytes[..bytes.len() - 1]).is_err());
        }
        assert!(Lookup::load(&mut &[8][..]).is_err());
    }
}
