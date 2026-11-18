#pragma once

// Dates and time zones.

#include "engine/api/GlobalObject.h"

namespace JSC {

class DateInstance final : public JSNonFinalObject {
public:
    ENGINE_DECLARE_CLASS(DateInstance)

    static DateInstance* create(VM& vm, Structure* structure, double time) { return Engine::date_create(vm, structure, time); }

    double internalNumber() const { return Engine::fieldAt<double>(this, Engine::Layout::DateInstance_time); }
};
ENGINE_CLASS_HAS_TYPE(DateInstance, JSDateType);

inline void DateCache::clearForTimeZoneChange() { Engine::vm_follow_time_zone(vm()); }
inline double DateCache::gregorianDateTimeToMS(int32_t year, int32_t month, int32_t day, int32_t hour, int32_t minute, int32_t second, int32_t millisecond, TimeType type) { return Engine::date_from_components(vm(), std::array { year, month, day, hour, minute, second, millisecond }, type == TimeType::LocalTime); }
inline PlainGregorianDateTime DateCache::msToGregorianDateTime(double time, TimeType type) { return PlainGregorianDateTime(Engine::date_fields(vm(), time, type == TimeType::LocalTime)); }
inline double DateCache::parseDate(JSGlobalObject*, VM& vm, const String& text) { return Engine::date_parse(vm, text); }

inline JSValue dateNowImpl(JSGlobalObject* globalObject) { return jsNumber(globalObject->jsDateNow()); }
// As `Date.prototype.toUTCString` does.
inline String formatDateTimeAsUTCString(double time) { return Engine::date_to_utc_string(time); }
// The same as `Date.parse`, but for text that names no zone, which is in UTC.
inline double parseDateAsUTC(std::span<const uint8_t> utf8) { return Engine::date_parse_as_utc(utf8); }

} // namespace JSC
