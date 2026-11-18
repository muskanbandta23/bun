#pragma once

#include "engine/support/KeyValuePair.h"
#include "engine/support/Vector.h"
#include "engine/text/String.h"

#include <optional>

// A setter leaves the URL as it is if the URL or the value is invalid.
extern "C" {
void engine_url_parse(WTF::URL& result, const WTF::StringImpl* input, const WTF::URL* base);
void engine_url_from_file_path(WTF::URL& result, WTF::StringView path);
WTF::StringImpl* engine_url_file_path(const WTF::URL&);
WTF::StringImpl* engine_url_decoded_username(const WTF::URL&);
WTF::StringImpl* engine_url_decoded_password(const WTF::URL&);
WTF::StringImpl* engine_url_scheme_and_host(const WTF::URL&) RETURNS_NONNULL;
WTF::StringImpl* engine_url_string_for_message(const WTF::URL&) RETURNS_NONNULL;
void engine_url_set_protocol(WTF::URL&, WTF::StringView);
void engine_url_set_username(WTF::URL&, WTF::StringView);
void engine_url_set_password(WTF::URL&, WTF::StringView);
void engine_url_remove_credentials(WTF::URL&);
bool engine_url_set_hostname(WTF::URL&, WTF::StringView);
void engine_url_set_host(WTF::URL&, WTF::StringView);
void engine_url_set_port(WTF::URL&, WTF::StringView);
void engine_url_set_pathname(WTF::URL&, WTF::StringView);
void engine_url_set_query(WTF::URL&, WTF::StringView);
void engine_url_set_fragment(WTF::URL&, WTF::StringView);
bool engine_url_is_special_scheme(WTF::StringView);
bool engine_url_is_default_port(uint16_t port, WTF::StringView scheme);
}

namespace WTF {

// `bun_url_whatwg::Url`.
class URL {
    WTF_MAKE_TZONE_ALLOCATED(URL);

public:
    URL() = default;
    explicit URL(const String& absolute) { engine_url_parse(*this, absolute.impl(), nullptr); }
    URL(const URL& base, const String& relative) { engine_url_parse(*this, relative.impl(), &base); }

    URL(const URL&) = default;
    URL& operator=(const URL&) = default;

    URL(URL&& other)
        : m_string(WTF::move(other.m_string))
        , m_parts(std::exchange(other.m_parts, {}))
    {
    }

    URL& operator=(URL&& other)
    {
        m_string = WTF::move(other.m_string);
        m_parts = std::exchange(other.m_parts, {});
        return *this;
    }

    static URL fileURLWithFileSystemPath(StringView path)
    {
        URL url;
        engine_url_from_file_path(url, path);
        return url;
    }

    URL isolatedCopy() const
    {
        URL copy;
        copy.m_string = m_string.isolatedCopy();
        copy.m_parts = m_parts;
        return copy;
    }

    bool isNull() const { return m_string.isNull(); }
    bool isEmpty() const { return m_string.isEmpty(); }
    bool isValid() const { return m_parts.state != State::Invalid; }
    bool hasOpaquePath() const { return m_parts.state == State::OpaquePath; }

    const String& string() const& LIFETIME_BOUND { return m_string; }
    String string() &&
    {
        m_parts = {};
        return WTF::move(m_string);
    }
    String stringWithoutFragmentIdentifier() const { return isValid() ? m_string.left(m_parts.queryEnd) : m_string; }
    String stringCenterEllipsizedToLength() const { return adoptRef(*engine_url_string_for_message(*this)); }

    StringView protocol() const LIFETIME_BOUND { return part(0, m_parts.schemeEnd); }
    StringView encodedUser() const LIFETIME_BOUND { return part(std::min(m_parts.usernameEnd, m_parts.schemeEnd + 3), m_parts.usernameEnd); }
    StringView encodedPassword() const LIFETIME_BOUND { return m_parts.hostStart > m_parts.usernameEnd + 1 ? part(m_parts.usernameEnd + 1, m_parts.hostStart - 1) : StringView {}; }
    StringView host() const LIFETIME_BOUND { return part(m_parts.hostStart, m_parts.hostEnd); }
    std::optional<uint16_t> port() const { return hasHost() && m_parts.pathStart > m_parts.hostEnd ? std::optional { m_parts.port } : std::nullopt; }
    StringView path() const LIFETIME_BOUND { return part(m_parts.pathStart, m_parts.pathEnd); }
    StringView query() const LIFETIME_BOUND { return hasQuery() ? part(m_parts.pathEnd + 1, m_parts.queryEnd) : StringView {}; }
    StringView queryWithLeadingQuestionMark() const LIFETIME_BOUND { return hasQuery() ? part(m_parts.pathEnd, m_parts.queryEnd) : StringView {}; }
    StringView fragmentIdentifier() const LIFETIME_BOUND { return hasFragmentIdentifier() ? part(m_parts.queryEnd + 1, m_string.length()) : StringView {}; }
    StringView fragmentIdentifierWithLeadingNumberSign() const LIFETIME_BOUND { return hasFragmentIdentifier() ? part(m_parts.queryEnd, m_string.length()) : StringView {}; }

    unsigned pathStart() const { return m_parts.pathStart; }

    String hostAndPort() const { return part(m_parts.hostStart, hasHost() ? m_parts.pathStart : m_parts.hostEnd).toString(); }
    String protocolHostAndPort() const { return adoptRef(*engine_url_scheme_and_host(*this)); }
    // These are null if what they decode to is not UTF-8.
    String user() const { return adoptRef(engine_url_decoded_username(*this)); }
    String password() const { return encodedPassword().isNull() ? String {} : adoptRef(engine_url_decoded_password(*this)); }
    String fileSystemPath() const { return adoptRef(engine_url_file_path(*this)); }

    bool hasCredentials() const { return m_parts.hostStart > m_parts.schemeEnd + 3; }
    bool hasQuery() const { return m_parts.queryEnd > m_parts.pathEnd; }
    bool hasFragmentIdentifier() const { return isValid() && m_string.length() > m_parts.queryEnd; }

    // `protocol` is in lowercase.
    ALWAYS_INLINE bool protocolIs(StringView protocol) const { return isValid() && this->protocol() == protocol; }
    bool protocolIsBlob() const { return protocolIs("blob"_s); }
    bool protocolIsJavaScript() const { return protocolIs("javascript"_s); }
    bool protocolIsFile() const { return m_parts.scheme == Scheme::File; }
    bool protocolIsInFTPFamily() const { return m_parts.scheme == Scheme::Ftp || protocolIs("ftps"_s); }
    bool protocolIsInHTTPFamily() const { return m_parts.scheme == Scheme::Http || m_parts.scheme == Scheme::Https; }
    bool hasSpecialScheme() const { return m_parts.scheme != Scheme::Other; }

    void setProtocol(StringView value) { engine_url_set_protocol(*this, value); }
    void setUser(StringView value) { engine_url_set_username(*this, value); }
    void setPassword(StringView value) { engine_url_set_password(*this, value); }
    void removeCredentials() { engine_url_remove_credentials(*this); }
    bool setHost(StringView value) { return engine_url_set_hostname(*this, value); }
    void setHostAndPort(StringView value) { engine_url_set_host(*this, value); }
    void setPort(std::optional<uint16_t> port) { engine_url_set_port(*this, port ? String::number(*port) : String {}); }
    void setPath(StringView value) { engine_url_set_pathname(*this, value); }
    // The leading question mark is optional. Null removes the query.
    void setQuery(StringView value) { engine_url_set_query(*this, value.startsWith('?') ? value.substring(1) : value); }
    void setFragmentIdentifier(StringView value) { engine_url_set_fragment(*this, value.isNull() ? ""_s : value); }
    void removeFragmentIdentifier()
    {
        if (isValid())
            m_string = m_string.left(m_parts.queryEnd);
    }

private:
    enum class Scheme : uint8_t { Other,
        Http,
        Https,
        Ws,
        Wss,
        Ftp,
        File };
    enum class State : uint8_t { Invalid,
        Hierarchical,
        OpaquePath };

    struct Parts {
        unsigned schemeEnd;
        unsigned usernameEnd;
        unsigned hostStart;
        unsigned hostEnd;
        unsigned pathStart;
        unsigned pathEnd;
        unsigned queryEnd;
        uint16_t port;
        Scheme scheme;
        State state;
    };

    bool hasHost() const { return m_parts.hostStart > m_parts.schemeEnd + 1; }
    StringView part(unsigned start, unsigned end) const LIFETIME_BOUND { return isValid() ? m_string.impl()->span8().subspan(start, end - start) : StringView {}; }

    String m_string;
    Parts m_parts {};
};

static_assert(sizeof(URL) == 40);

template<> struct VectorTraits<URL> : MovableClassVectorTraits {};

inline bool operator==(const URL& a, const URL& b) { return a.string() == b.string(); }

inline bool isDefaultPortForProtocol(uint16_t port, StringView protocol) { return engine_url_is_default_port(port, protocol); }

class URLParser {
public:
    using URLEncodedForm = Vector<KeyValuePair<String, String>>;

    // Fails if the list or the UTF-16 of a name or a value would take more than `sizeLimit` bytes.
    static std::optional<URLEncodedForm> tryParseURLEncodedForm(StringView, size_t sizeLimit);
    static String serialize(const URLEncodedForm&);
    static bool isSpecialScheme(StringView scheme) { return engine_url_is_special_scheme(scheme); }
};

}
