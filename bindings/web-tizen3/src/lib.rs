//! Pointer-only transport for Binaryen's wasm2js. Policy remains in `den-sync::evaluate_core`.
use std::{mem, slice, str};

#[no_mangle]
pub fn den_alloc(len: usize) -> *mut u8 {
    let mut value = Vec::<u8>::with_capacity(len);
    let pointer = value.as_mut_ptr();
    mem::forget(value);
    pointer
}

/// # Safety
/// `pointer` and `len` must be a pair returned by `den_alloc` or `den_evaluate` and not yet freed.
#[no_mangle]
pub unsafe fn den_free(pointer: *mut u8, len: usize) {
    if len != 0 {
        drop(Vec::from_raw_parts(pointer, len, len));
    }
}

/// # Safety
/// The input must name `input_len` initialized UTF-8 bytes and `output` must name two writable `usize`s.
#[no_mangle]
pub unsafe fn den_evaluate(input_pointer: *const u8, input_len: usize, output: *mut usize) {
    let input = str::from_utf8_unchecked(slice::from_raw_parts(input_pointer, input_len));
    let mut answer = den_sync::evaluate_core(input).into_bytes().into_boxed_slice();
    *output = answer.as_mut_ptr() as usize;
    *output.add(1) = answer.len();
    mem::forget(answer);
}
