function from(items /*, mapFn, thisArg */) {
  if (!@isConstructor(this)) @throwTypeError("TypedArray.from requires |this| to be a constructor");
  var mapFn = @argument(1);
  var thisArg = @argument(2);
  if (mapFn !== @undefined && !@isCallable(mapFn))
    @throwTypeError("TypedArray.from requires that the second argument, when provided, be a function");
  var source = @toObject(items, "TypedArray.from requires an array-like object - not null or undefined");
  if (mapFn === @undefined) {
    var fastResult = @typedArrayFromFast(this, source);
    if (fastResult) return fastResult;
  }

  var length = 0;
  var iteratorMethod = items.@@iterator;
  if (@isUndefinedOrNull(iteratorMethod)) length = @toLength(source.length);
  else {
    if (!@isCallable(iteratorMethod))
      @throwTypeError(
        "TypedArray.from requires that the property of the first argument, items[Symbol.iterator], when exists, be a function",
      );
    var iterator = iteratorMethod.@call(items);
    if (!@isObject(iterator)) @throwTypeError("Iterator result interface is not an object.");
    var nextMethod = iterator.next;
    source = [];
    for (;;) {
      var next = @iteratorGenericNext(nextMethod, iterator);
      if (next.done) break;
      @putByValDirect(source, length++, next.value);
    }
  }

  var result = new this(length);
  if (@typedArrayLength(result) < length)
    @throwTypeError("TypedArray.from constructed typed array of insufficient length");
  if (mapFn === @undefined) {
    for (var i = 0; i < length; i++) result[i] = source[i];
  } else {
    for (var i = 0; i < length; i++) result[i] = mapFn.@call(thisArg, source[i], i);
  }
  return result;
}
