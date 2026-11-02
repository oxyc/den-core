//! One lookup type and one sort for whole-library lists. Each distinct `BTreeMap` or slice-sort instantiation is
//! kilobytes of every browser module that holds it, and a Worker compiles its own copy of each module, so lists look up
//! positions (`Index`) and sort positions (`sort`) rather than their rows.

use std::cmp::Ordering;
use std::collections::BTreeMap;

/// A lookup to a position: by name (`String`) or by a number (`u64`, a TMDB id or a title key's code).
pub(crate) type Index<K> = BTreeMap<K, usize>;

/// `0..n`, to be sorted by `sort` and applied by `permute`.
pub(crate) fn indices(n: usize) -> Vec<usize> {
    (0..n).collect()
}

/// A stable sort of positions by `cmp`: a bottom-up merge sort, a few hundred bytes where the standard library's sort
/// is over ten kilobytes in each module that instantiates it.
pub(crate) fn sort(order: &mut [usize], cmp: &dyn Fn(usize, usize) -> Ordering) {
    let n = order.len();
    let mut from = order.to_vec();
    let mut to = vec![0; n];
    let mut width = 1;
    while width < n {
        for start in (0..n).step_by(2 * width) {
            let middle = (start + width).min(n);
            let end = (start + 2 * width).min(n);
            let (mut left, mut right) = (start, middle);
            for slot in &mut to[start..end] {
                // Equal keys take the left run first, which keeps the sort stable.
                let take_left = right >= end
                    || (left < middle && cmp(from[left], from[right]) != Ordering::Greater);
                *slot = if take_left { from[left] } else { from[right] };
                if take_left {
                    left += 1;
                } else {
                    right += 1;
                }
            }
        }
        std::mem::swap(&mut from, &mut to);
        width *= 2;
    }
    order.copy_from_slice(&from);
}

/// `rows` in `order` (a permutation of their positions).
pub(crate) fn permute<T>(rows: Vec<T>, order: &[usize]) -> Vec<T> {
    let mut rows: Vec<Option<T>> = rows.into_iter().map(Some).collect();
    order
        .iter()
        .map(|&i| rows[i].take().expect("a permutation"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_is_the_standard_stable_sort() {
        // A fixed pseudo-random sequence with many equal keys, at every length up to 300.
        let mut x = 0x2545_f491_u64;
        for n in 0..300 {
            let keys: Vec<u64> = (0..n)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x % 7
                })
                .collect();
            let mut ours = indices(n);
            sort(&mut ours, &|a, b| keys[a].cmp(&keys[b]));
            let mut std = indices(n);
            std.sort_by_key(|&i| keys[i]);
            assert_eq!(ours, std, "{n}");
        }
    }
}
