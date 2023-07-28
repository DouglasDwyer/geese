/// Evaluates the provided expression in a static context on nightly, and during runtime on stable.
macro_rules! static_eval {
    ($assertion: expr, $result_type: ty, $($type_parameter: ident $(,)?)*) => {
        {
            struct StaticEval<$($type_parameter: GeeseSystem, )*>(PhantomData<fn($($type_parameter, )*)>);

            #[cfg(unstable)]
            impl<$($type_parameter: GeeseSystem, )*> StaticEval<$($type_parameter, )*> {
                const VALUE: $result_type = $assertion;

                #[inline(always)]
                const fn calculate() -> $result_type {
                    Self::VALUE
                }
            }

            #[cfg(not(unstable))]
            impl<$($type_parameter: GeeseSystem, )*> StaticEval<$($type_parameter, )*> {
                #[inline(always)]
                const fn calculate() -> $result_type {
                    $assertion
                }
            }

            StaticEval::<$($type_parameter, )*>::calculate()
        }
    };
}

/// Evaluates the provided expression in a static context.
macro_rules! const_eval {
    ($assertion: expr, $result_type: ty, $($type_parameter: ident $(,)?)*) => {
        {
            struct StaticEval<$($type_parameter: GeeseSystem, )*>(PhantomData<fn($($type_parameter, )*)>);

            impl<$($type_parameter: GeeseSystem, )*> StaticEval<$($type_parameter, )*> {
                const VALUE: $result_type = $assertion;

                #[inline(always)]
                const fn calculate() -> $result_type {
                    Self::VALUE
                }
            }

            StaticEval::<$($type_parameter, )*>::calculate()
        }
    };
}

/// Attempts to unwrap an option in a `const` context. If the option has no value, panics.
pub const fn const_unwrap<T: Copy>(value: Option<T>) -> T {
    if let Some(result) = value {
        result
    } else {
        panic!("Constant unwrap failed.")
    }
}

pub(crate) use const_eval;
pub(crate) use static_eval;
