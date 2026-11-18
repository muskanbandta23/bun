#include "engine/text/URL.h"

using Pair = WTF::KeyValuePair<WTF::String, WTF::String>;

extern "C" {
// `reserve` returns room for so many pairs, or null.
bool engine_url_parse_form(WTF::StringView, size_t sizeLimit, void* context, Pair* (*reserve)(void* context, size_t count));
WTF::StringImpl* engine_url_serialize_form(const Pair*, size_t count) RETURNS_NONNULL;
}

namespace WTF {

auto URLParser::tryParseURLEncodedForm(StringView input, size_t sizeLimit) -> std::optional<URLEncodedForm>
{
    URLEncodedForm form;
    auto reserve = [](void* context, size_t count) {
        auto& form = *static_cast<URLEncodedForm*>(context);
        return form.tryGrow(count) ? form.mutableSpan().data() : nullptr;
    };
    if (!engine_url_parse_form(input, sizeLimit, &form, reserve))
        return std::nullopt;
    return form;
}

String URLParser::serialize(const URLEncodedForm& form)
{
    if (form.isEmpty())
        return {};
    return adoptRef(*engine_url_serialize_form(form.span().data(), form.size()));
}

}
