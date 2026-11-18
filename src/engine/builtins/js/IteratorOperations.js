// @private
function collectForSpread(iterator, next) {
  var values = [];
  var count = 0;
  for (;;) {
    var result = next.@call(iterator);
    if (!@isObject(result)) @throwTypeError("Iterator result interface is not an object");
    if (result.done) return values;
    @putByValDirect(values, count++, result.value);
  }
}
