// Translated from C to Rust. The original C code can be found at
// https://github.com/ulfjack/ryu and carries the following license:
//
// Copyright 2018 Ulf Adams
//
// The contents of this file may be used under the terms of the Apache License,
// Version 2.0.
//
//    (See accompanying file LICENSE-Apache or copy at
//     http://www.apache.org/licenses/LICENSE-2.0)
//
// Alternatively, the contents of this file may be used under the terms of
// the Boost Software License, Version 1.0.
//    (See accompanying file LICENSE-Boost or copy at
//     https://www.boost.org/LICENSE_1_0.txt)
//
// Unless required by applicable law or agreed to in writing, this software
// is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.

use crate::common::*;
use crate::d2s;

pub const FLOAT_MANTISSA_BITS: u32 = 23;
pub const FLOAT_EXPONENT_BITS: u32 = 8;

const FLOAT_BIAS: i32 = 127;
const FLOAT_POW5_INV_BITCOUNT: i32 = d2s::DOUBLE_POW5_INV_BITCOUNT - 64;
const FLOAT_POW5_BITCOUNT: i32 = d2s::DOUBLE_POW5_BITCOUNT - 64;

#[cfg_attr(feature = "no-panic", inline)]
fn pow5factor_32(mut value: u32) -> u32 {
    let mut count = 0u32;
    loop {
        debug_assert!(value != 0);
        let q = value / 5;
        let r = value % 5;
        if r != 0 {
            break;
        }
        value = q;
        count += 1;
    }
    count
}

// Returns true if value is divisible by 5^p.
#[cfg_attr(feature = "no-panic", inline)]
fn multiple_of_power_of_5_32(value: u32, p: u32) -> bool {
    pow5factor_32(value) >= p
}

// Returns true if value is divisible by 2^p.
#[cfg_attr(feature = "no-panic", inline)]
fn multiple_of_power_of_2_32(value: u32, p: u32) -> bool {
    // return __builtin_ctz(value) >= p;
    (value & ((1u32 << p) - 1)) == 0
}

// It seems to be slightly faster to avoid uint128_t here, although the
// generated code for uint128_t looks slightly nicer.
#[cfg_attr(feature = "no-panic", inline)]
fn mul_shift(m: u32, factor: u64, shift: i32) -> u32 {
    debug_assert!(shift > 32);

    // The casts here help MSVC to avoid calls to the __allmul library
    // function.
    let factor_lo = factor as u32;
    let factor_hi = (factor >> 32) as u32;
    let bits0 = m as u64 * factor_lo as u64;
    let bits1 = m as u64 * factor_hi as u64;

    let sum = (bits0 >> 32) + bits1;
    let shifted_sum = sum >> (shift - 32);
    debug_assert!(shifted_sum <= u32::max_value() as u64);
    shifted_sum as u32
}

#[cfg_attr(feature = "no-panic", inline)]
fn mul_pow5_inv_div_pow2(m: u32, q: u32, j: i32) -> u32 {
    #[cfg(feature = "small")]
    {
        // The inverse multipliers are defined as [2^x / 5^y] + 1; the upper 64
        // bit from the double lookup table are the correct bits for [2^x /
        // 5^y], so we have to add 1 here. Note that we rely on the fact that
        // the added 1 that's already stored in the table never overflows into
        // the upper 64 bit.
        let pow5 = unsafe { d2s::compute_inv_pow5(q) };
        mul_shift(m, pow5.1 + 1, j)
    }

    #[cfg(not(feature = "small"))]
    {
        debug_assert!(q < d2s::DOUBLE_POW5_INV_SPLIT.len() as u32);
        unsafe {
            mul_shift(
                m,
                d2s::DOUBLE_POW5_INV_SPLIT.get_unchecked(q as usize).1 + 1,
                j,
            )
        }
    }
}

#[cfg_attr(feature = "no-panic", inline)]
fn mul_pow5_div_pow2(m: u32, i: u32, j: i32) -> u32 {
    #[cfg(feature = "small")]
    {
        let pow5 = unsafe { d2s::compute_pow5(i) };
        mul_shift(m, pow5.1, j)
    }

    #[cfg(not(feature = "small"))]
    {
        debug_assert!(i < d2s::DOUBLE_POW5_SPLIT.len() as u32);
        unsafe { mul_shift(m, d2s::DOUBLE_POW5_SPLIT.get_unchecked(i as usize).1, j) }
    }
}

// A floating decimal representing m * 10^e.
pub struct FloatingDecimal32 {
    pub mantissa: u32,
    // Decimal exponent's range is -45 to 38
    // inclusive, and can fit in i16 if needed.
    pub exponent: i32,
}

#[cfg_attr(feature = "no-panic", inline)]
pub fn f2d(ieee_mantissa: u32, ieee_exponent: u32) -> FloatingDecimal32 {
    let (e2, m2) = if ieee_exponent == 0 {
        (
            // We subtract 2 so that the bounds computation has 2 additional bits.
            1 - FLOAT_BIAS - FLOAT_MANTISSA_BITS as i32 - 2,
            ieee_mantissa,
        )
    } else {
        (
            ieee_exponent as i32 - FLOAT_BIAS - FLOAT_MANTISSA_BITS as i32 - 2,
            (1u32 << FLOAT_MANTISSA_BITS) | ieee_mantissa,
        )
    };
    let even = (m2 & 1) == 0;
    let accept_bounds = even;

    // Step 2: Determine the interval of valid decimal representations.
    let mv = 4 * m2;
    let mp = 4 * m2 + 2;
    // Implicit bool -> int conversion. True is 1, false is 0.
    let mm_shift = (ieee_mantissa != 0 || ieee_exponent <= 1) as u32;
    let mm = 4 * m2 - 1 - mm_shift;

    // Step 3: Convert to a decimal power base using 64-bit arithmetic.
    let mut vr: u32;
    let mut vp: u32;
    let mut vm: u32;
    let e10: i32;
    let mut vm_is_trailing_zeros = false;
    let mut vr_is_trailing_zeros = false;
    let mut last_removed_digit = 0u8;
    if e2 >= 0 {
        let q = log10_pow2(e2);
        e10 = q as i32;
        let k = FLOAT_POW5_INV_BITCOUNT + pow5bits(q as i32) - 1;
        let i = -e2 + q as i32 + k;
        vr = mul_pow5_inv_div_pow2(mv, q, i);
        vp = mul_pow5_inv_div_pow2(mp, q, i);
        vm = mul_pow5_inv_div_pow2(mm, q, i);
        if q != 0 && (vp - 1) / 10 <= vm / 10 {
            // We need to know one removed digit even if we are not going to loop below. We could use
            // q = X - 1 above, except that would require 33 bits for the result, and we've found that
            // 32-bit arithmetic is faster even on 64-bit machines.
            let l = FLOAT_POW5_INV_BITCOUNT + pow5bits(q as i32 - 1) - 1;
            last_removed_digit =
                (mul_pow5_inv_div_pow2(mv, q - 1, -e2 + q as i32 - 1 + l) % 10) as u8;
        }
        if q <= 9 {
            // The largest power of 5 that fits in 24 bits is 5^10, but q <= 9 seems to be safe as well.
            // Only one of mp, mv, and mm can be a multiple of 5, if any.
            if mv % 5 == 0 {
                vr_is_trailing_zeros = multiple_of_power_of_5_32(mv, q);
            } else if accept_bounds {
                vm_is_trailing_zeros = multiple_of_power_of_5_32(mm, q);
            } else {
                vp -= multiple_of_power_of_5_32(mp, q) as u32;
            }
        }
    } else {
        let q = log10_pow5(-e2);
        e10 = q as i32 + e2;
        let i = -e2 - q as i32;
        let k = pow5bits(i) - FLOAT_POW5_BITCOUNT;
        let mut j = q as i32 - k;
        vr = mul_pow5_div_pow2(mv, i as u32, j);
        vp = mul_pow5_div_pow2(mp, i as u32, j);
        vm = mul_pow5_div_pow2(mm, i as u32, j);
        if q != 0 && (vp - 1) / 10 <= vm / 10 {
            j = q as i32 - 1 - (pow5bits(i + 1) - FLOAT_POW5_BITCOUNT);
            last_removed_digit = (mul_pow5_div_pow2(mv, (i + 1) as u32, j) % 10) as u8;
        }
        if q <= 1 {
            // {vr,vp,vm} is trailing zeros if {mv,mp,mm} has at least q trailing 0 bits.
            // mv = 4 * m2, so it always has at least two trailing 0 bits.
            vr_is_trailing_zeros = true;
            if accept_bounds {
                // mm = mv - 1 - mm_shift, so it has 1 trailing 0 bit iff mm_shift == 1.
                vm_is_trailing_zeros = mm_shift == 1;
            } else {
                // mp = mv + 2, so it always has at least one trailing 0 bit.
                vp -= 1;
            }
        } else if q < 31 {
            // TODO(ulfjack): Use a tighter bound here.
            vr_is_trailing_zeros = multiple_of_power_of_2_32(mv, q - 1);
        }
    }

    // Step 4: Find the shortest decimal representation in the interval of valid representations.
    let mut removed = 0i32;
    let output = if vm_is_trailing_zeros || vr_is_trailing_zeros {
        // General case, which happens rarely (~4.0%).
        while vp / 10 > vm / 10 {
            vm_is_trailing_zeros &= vm - (vm / 10) * 10 == 0;
            vr_is_trailing_zeros &= last_removed_digit == 0;
            last_removed_digit = (vr % 10) as u8;
            vr /= 10;
            vp /= 10;
            vm /= 10;
            removed += 1;
        }
        if vm_is_trailing_zeros {
            while vm % 10 == 0 {
                vr_is_trailing_zeros &= last_removed_digit == 0;
                last_removed_digit = (vr % 10) as u8;
                vr /= 10;
                vp /= 10;
                vm /= 10;
                removed += 1;
            }
        }
        if vr_is_trailing_zeros && last_removed_digit == 5 && vr % 2 == 0 {
            // Round even if the exact number is .....50..0.
            last_removed_digit = 4;
        }
        // We need to take vr + 1 if vr is outside bounds or we need to round up.
        vr + ((vr == vm && (!accept_bounds || !vm_is_trailing_zeros)) || last_removed_digit >= 5)
            as u32
    } else {
        // Specialized for the common case (~96.0%). Percentages below are relative to this.
        // Loop iterations below (approximately):
        // 0: 13.6%, 1: 70.7%, 2: 14.1%, 3: 1.39%, 4: 0.14%, 5+: 0.01%
        while vp / 10 > vm / 10 {
            last_removed_digit = (vr % 10) as u8;
            vr /= 10;
            vp /= 10;
            vm /= 10;
            removed += 1;
        }
        // We need to take vr + 1 if vr is outside bounds or we need to round up.
        vr + (vr == vm || last_removed_digit >= 5) as u32
    };
    let exp = e10 + removed;

    FloatingDecimal32 {
        exponent: exp,
        mantissa: output,
    }
}
