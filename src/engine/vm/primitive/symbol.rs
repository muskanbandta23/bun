use bun_alloc::AllocError;
use bun_engine_macros::cell;
use bun_engine_text::{PropertyName, StringImpl, StringRef, SymbolRef, Text};

use crate::{Cx, Field, Gc, JsCell, JsResult, JsString, TypeFlags, Value, Vm};

#[cell(name = "symbol", cell_type = Symbol, flags = TypeFlags::SHAPE_IS_IMMORTAL,
    export = "engine_class_Symbol")]
pub struct JsSymbol {
    base: JsCell,
    symbol: SymbolRef,
    pub(self) cached_description: Field<Option<Gc<JsString>>>,
    pub(self) cached_string: Field<Option<Gc<JsString>>>,
}

impl JsSymbol {
    pub const OFFSET_OF_NAME: usize = core::mem::offset_of!(JsSymbol, symbol);

    /// There is no cell for the symbol.
    fn allocate(vm: &Vm, symbol: SymbolRef, description: Option<Gc<JsString>>) -> Gc<JsSymbol> {
        let address = symbol.name().address();
        let cell = vm.allocate::<JsSymbol>(JsSymbolInit {
            base: vm.shapes.symbol(),
            symbol,
            cached_description: description,
            cached_string: None,
        });
        vm.symbols.insert(address, cell);
        cell
    }

    /// The one cell that stands for the symbol.
    pub fn for_symbol(vm: &Vm, symbol: SymbolRef) -> Gc<JsSymbol> {
        (vm.symbols.get(vm, &symbol.name().address()))
            .unwrap_or_else(|| Self::allocate(vm, symbol, None))
    }

    pub fn create(vm: &Vm, description: Option<&StringImpl>) -> Gc<JsSymbol> {
        let symbol = description.map_or_else(SymbolRef::without_description, SymbolRef::new);
        Self::allocate(vm, symbol, None)
    }

    pub fn with_description(cx: &Cx, description: Gc<JsString>) -> JsResult<Gc<JsSymbol>> {
        let symbol = SymbolRef::new(&*description.resolve(cx)?);
        Ok(Self::allocate(cx, symbol, Some(description)))
    }

    pub fn create_private(vm: &Vm, description: &StringImpl) -> Gc<JsSymbol> {
        Self::allocate(vm, SymbolRef::private(description), None)
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

    pub fn to_descriptive_string(&self, cx: &Cx) -> JsResult<Gc<JsString>> {
        if let Some(string) = self.cached_string() {
            return Ok(string);
        }
        let string = JsString::new(cx, cx.or_out_of_memory(self.descriptive_string())?);
        self.set_cached_string(Some(string));
        Ok(string)
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
