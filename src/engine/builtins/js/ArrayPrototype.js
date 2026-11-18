function forEach(callback /*, thisArg */) {
  "use strict";
  var array = @toObject(this, "Array.prototype.forEach requires that |this| not be null or undefined");
  var length = @toLength(array.length);
  if (!@isCallable(callback)) @throwTypeError("Array.prototype.forEach callback must be a function");
  var thisArg = @argument(1);
  for (var i = 0; i < length; i++) {
    if (i in array) callback.@call(thisArg, array[i], i, array);
  }
}
