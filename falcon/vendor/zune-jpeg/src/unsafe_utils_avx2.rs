/*
 * Copyright (c) 2023.
 *
 * This software is free software;
 *
 * You can redistribute it or modify it under terms of the MIT, Apache License or Zlib license
 */

#![cfg(all(feature = "x86", any(target_arch = "x86", target_arch = "x86_64")))]
//! This module provides unsafe ways to do some things
#![allow(clippy::wildcard_imports)]

#[cfg(target_arch = "x86")]
use core::arch::x86::*;
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;
use core::ops::{Add, AddAssign, Mul, MulAssign, Sub};

/// A copy of `_MM_SHUFFLE()` that doesn't require
/// a nightly compiler
#[inline]
const fn shuffle(z: i32, y: i32, x: i32, w: i32) -> i32 {
    (z << 6) | (y << 4) | (x << 2) | w
}

/// An abstraction of an AVX ymm register that
///allows some things to not look ugly
#[derive(Clone, Copy)]
pub struct YmmRegister {
    /// An AVX register
    pub(crate) mm256: __m256i
}

impl Add for YmmRegister {
    type Output = YmmRegister;

    #[inline]
    fn add(self, rhs: Self) -> Self::Output {
        unsafe {
            return YmmRegister {
                mm256: _mm256_add_epi32(self.mm256, rhs.mm256)
            };
        }
    }
}

impl Add<i32> for YmmRegister {
    type Output = YmmRegister;

    #[inline]
    fn add(self, rhs: i32) -> Self::Output {
        unsafe {
            let tmp = _mm256_set1_epi32(rhs);

            return YmmRegister {
                mm256: _mm256_add_epi32(self.mm256, tmp)
            };
        }
    }
}

impl Sub for YmmRegister {
    type Output = YmmRegister;

    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        unsafe {
            return YmmRegister {
                mm256: _mm256_sub_epi32(self.mm256, rhs.mm256)
            };
        }
    }
}

impl AddAssign for YmmRegister {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        unsafe {
            self.mm256 = _mm256_add_epi32(self.mm256, rhs.mm256);
        }
    }
}

impl AddAssign<i32> for YmmRegister {
    #[inline]
    fn add_assign(&mut self, rhs: i32) {
        unsafe {
            let tmp = _mm256_set1_epi32(rhs);

            self.mm256 = _mm256_add_epi32(self.mm256, tmp);
        }
    }
}

impl Mul for YmmRegister {
    type Output = YmmRegister;

    #[inline]
    fn mul(self, rhs: Self) -> Self::Output {
        unsafe {
            YmmRegister {
                mm256: _mm256_mullo_epi32(self.mm256, rhs.mm256)
            }
        }
    }
}

impl Mul<i32> for YmmRegister {
    type Output = YmmRegister;

    #[inline]
    fn mul(self, rhs: i32) -> Self::Output {
        unsafe {
            let tmp = _mm256_set1_epi32(rhs);

            YmmRegister {
                mm256: _mm256_mullo_epi32(self.mm256, tmp)
            }
        }
    }
}

impl MulAssign for YmmRegister {
    #[inline]
    fn mul_assign(&mut self, rhs: Self) {
        unsafe {
            self.mm256 = _mm256_mullo_epi32(self.mm256, rhs.mm256);
        }
    }
}

impl MulAssign<i32> for YmmRegister {
    #[inline]
    fn mul_assign(&mut self, rhs: i32) {
        unsafe {
            let tmp = _mm256_set1_epi32(rhs);

            self.mm256 = _mm256_mullo_epi32(self.mm256, tmp);
        }
    }
}

impl MulAssign<__m256i> for YmmRegister {
    #[inline]
    fn mul_assign(&mut self, rhs: __m256i) {
        unsafe {
            self.mm256 = _mm256_mullo_epi32(self.mm256, rhs);
        }
    }
}

type Reg = YmmRegister;


// Falcon replaces the old transpose with a separately tested mathematical implementation.
include!("falcon_transpose.rs");

