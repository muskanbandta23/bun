//! What an `@name` means in the source of a built-in, besides the intrinsics of the bytecode generator, the slots of
//! the realm and the private functions in `builtins/js`.

use bun_engine_bytecode::BuiltinValue::{self, Double, Int32};
use bun_engine_bytecode::ResumeMode;
use bun_engine_text::{MAX_ARRAY_INDEX, MAX_LENGTH, PropertyName, private_names};

use crate::{CellType, MAX_SAFE_INTEGER};

const fn is(cell_type: CellType) -> BuiltinValue {
    BuiltinValue::CellTest(bun_engine_bytecode::CellType(cell_type.0))
}

macro_rules! builtin_values {
    ($($name:ident = $value:expr,)*) => {
        pub(super) const BUILTIN_VALUES: &[(PropertyName<'static>, BuiltinValue)] =
            &[$((private_names::$name, $value),)*];
    };
}

builtin_values! {
    isArrayIterator = is(CellType::ArrayIterator),
    isAsyncDisposableStack = is(CellType::AsyncDisposableStack),
    isDerivedArray = is(CellType::DerivedArray),
    isDisposableStack = is(CellType::DisposableStack),
    isGenerator = is(CellType::Generator),
    isIteratorHelper = is(CellType::IteratorHelper),
    isJSArray = is(CellType::Array),
    isMap = is(CellType::Map),
    isPromise = is(CellType::Promise),
    isProxyObject = is(CellType::Proxy),
    isRegExpObject = is(CellType::RegExpObject),
    isSet = is(CellType::Set),
    isShadowRealm = is(CellType::ShadowRealm),
    isWrapForValidIterator = is(CellType::WrapForValidIterator),

    MAX_ARRAY_INDEX = Double(MAX_ARRAY_INDEX as f64),
    MAX_SAFE_INTEGER = Double(MAX_SAFE_INTEGER),
    MAX_STRING_LENGTH = Int32(MAX_LENGTH as i32),

    GeneratorResumeModeNormal = Int32(ResumeMode::Normal as i32),
    GeneratorResumeModeReturn = Int32(ResumeMode::Return as i32),
    GeneratorResumeModeThrow = Int32(ResumeMode::Throw as i32),

    proxyFieldTarget = Int32(0),
    proxyFieldHandler = Int32(1),
}
