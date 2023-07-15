/// Evaluates the provided expression in a static context if the `static_check` feature is enabled.
/// Otherwise, evaluates the expression at runtime.
#[macro_export]
macro_rules! static_eval {
    ($assertion: expr, $result_type: ty, $($type_parameter: ident $(,)?)*) => {
        {
            struct StaticEval<$($type_parameter: GeeseSystem, )*>(PhantomData<fn($($type_parameter, )*)>);
            
            #[cfg(feature = "static_check")]
            impl<$($type_parameter: GeeseSystem, )*> StaticEval<$($type_parameter, )*> {
                const VALUE: $result_type = $assertion;
                
                #[inline(never)]
                const fn calculate() -> $result_type {
                    Self::VALUE
                }
            }
    
            #[cfg(not(feature = "static_check"))]
            impl<$($type_parameter: GeeseSystem, )*> StaticEval<$($type_parameter, )*> {
                #[inline(never)]
                const fn calculate() -> $result_type {
                    $assertion
                }
            }
    
            StaticEval::<$($type_parameter, )*>::calculate()
        }
    };
}