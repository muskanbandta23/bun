function forEach(callback /*, thisArg */) {
  if (!@isMap(this)) @throwTypeError("Map operation called on non-Map object");
  if (!@isCallable(callback)) @throwTypeError("Map.prototype.forEach callback must be a function");
  var thisArg = @argument(1);
  var place = @mapStorage(this);
  var next = 0;
  while ((place = @mapIterationNext(place, next)) !== @undefined) {
    next = @mapIterationEntry(place) + 1;
    callback.@call(thisArg, @mapIterationEntryValue(place), @mapIterationEntryKey(place), this);
  }
}
