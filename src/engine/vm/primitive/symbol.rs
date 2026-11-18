use bun_alloc::AllocError;
use bun_engine_macros::cell;
use bun_engine_text::{PropertyName, StringImpl, StringRef, SymbolRef, Text};

use crate::{Field, Gc, JsCell, JsString, TypeFlags, Value, Vm};

#[cell(name = "symbol", cell_type = Symbol, flags = TypeFlags::SHAPE_IS_IMMORTAL,
    export = "engine_class_Symbol")]
pub struct JsSymbol {
    base: JsCell,
    symbol: SymbolRef,
    pub(self) cached_description: Field<Option<Gc<JsString>>>,
}

impl JsSymbol {
    pub const OFFSET_OF_NAME: usize = core::mem::offset_of!(JsSymbol, symbol);

    /// The one cell that stands for the symbol.
    pub fn for_symbol(vm: &Vm, symbol: SymbolRef) -> Gc<JsSymbol> {
        let address = symbol.name().address();
        vm.symbols.get(vm, &address).unwrap_or_else(|| {
            let cell = vm.allocate::<JsSymbol>(JsSymbolInit {
                base: vm.shapes.symbol(),
                symbol,
                cached_description: None,
            });
            vm.symbols.insert(address, cell);
            cell
        })
    }

    pub fn create(vm: &Vm, description: Option<&StringImpl>) -> Gc<JsSymbol> {
        let symbol = description.map_or_else(SymbolRef::without_description, SymbolRef::new);
        Self::for_symbol(vm, symbol)
    }

    pub fn create_private(vm: &Vm, description: &StringImpl) -> Gc<JsSymbol> {
        Self::for_symbol(vm, SymbolRef::private(description))
    }

    #[inline(always)]
    pub fn name(&self) -> PropertyName<'_> {
        self.symbol.name()
    }

    pub fn symbol(&self) -> &SymbolRef {
        &self.symbol
    }

    pub fn description(&self, vm: &Vm) -> Option<Gc<JsString>> {
        if self.cached_description().is_none() {
            let description = self.symbol.description_string()?;
            self.set_cached_description(Some(JsString::new(vm, description)));
        }
        self.cached_description()
    }

    /// SymbolDescriptiveString
    pub fn descriptive_string(&self) -> Result<StringRef, AllocError> {
        let (open, close) = (Text::from_ascii("Symbol("), Text::from_ascii(")"));
        StringRef::try_concat(&[open, self.name().text(), close])
    }
}

impl Value {
    /// The string or symbol that a script sees for a key.
    #[inline]
    pub fn from_name(vm: &Vm, name: PropertyName<'_>) -> Value {
        match name.to_symbol() {
            Some(symbol) => JsSymbol::for_symbol(vm, symbol).into(),
            None => JsString::from_name(vm, name).into(),
        }
    }
}
