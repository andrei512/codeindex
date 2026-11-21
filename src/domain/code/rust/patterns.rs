use super::*;

/// Introduce the bindings of each match arm for its guard and expression.
/// A block arm's comma is optional; expression arms terminate at a top-level comma.
pub(super) fn arms(
    t: &[Token],
    pairs: &[usize],
    open: usize,
    scope: usize,
    a: &mut Analysis,
    e: &mut Evidence,
    excluded: &mut [bool],
    starts: &mut HashMap<usize, usize>,
    ends: &mut HashMap<usize, usize>,
) {
    let close = pairs[open];
    if close == usize::MAX {
        return;
    }
    let mut first = open + 1;
    while first < close {
        if word(t, first) == "," {
            first += 1;
            continue;
        }
        let mut arrow = first;
        let mut guard = None;
        while arrow < close && !(word(t, arrow) == "=" && word(t, arrow + 1) == ">") {
            if matches!(word(t, arrow), "(" | "[" | "{") && pairs[arrow] != usize::MAX {
                arrow = pairs[arrow] + 1;
                continue;
            }
            if word(t, arrow) == "if" && guard.is_none() {
                guard = Some(arrow);
            }
            arrow += 1;
        }
        if arrow >= close {
            break;
        }
        let expr = arrow + 2;
        let end = if word(t, expr) == "{" && pairs[expr] != usize::MAX {
            pairs[expr] + 1
        } else {
            until(t, pairs, expr, &[",", "}"]).min(close)
        };
        let arm = new_scope(e, scope);
        bind_pattern(
            a,
            e,
            t,
            excluded,
            arm,
            first..guard.unwrap_or(arrow),
            "local_binding",
            0,
        );
        starts.insert(first, arm);
        ends.insert(end, scope);
        first = end;
    }
}
