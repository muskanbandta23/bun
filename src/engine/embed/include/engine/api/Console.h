#pragma once

#include "engine/api/GlobalObject.h"

namespace JSC {

class ConsoleClient {
public:
    virtual ~ConsoleClient() = default;
};

} // namespace JSC
