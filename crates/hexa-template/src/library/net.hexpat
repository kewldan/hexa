// Network protocol headers (big endian on the wire): Ethernet II with
// 802.1Q tags, IPv4, TCP and UDP.

enum EtherType : u16 {
    IPv4 = 0x0800,
    ARP = 0x0806,
    VLAN = 0x8100,
    IPv6 = 0x86DD,
    QinQ = 0x88A8
};

enum IpProtocol : u8 {
    ICMP = 1,
    IGMP = 2,
    TCP = 6,
    UDP = 17,
    IPv6 = 41,
    GRE = 47,
    ESP = 50,
    AH = 51,
    ICMPv6 = 58,
    SCTP = 132
};

struct MacAddress {
    u8 bytes[6];
} [[format("net_mac_text")]];

fn net_mac_text(auto m) {
    return std::format("{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}", m.bytes[0], m.bytes[1],
        m.bytes[2], m.bytes[3], m.bytes[4], m.bytes[5]);
};

struct IPv4Address {
    u8 octets[4];
} [[format("net_ipv4_text")]];

fn net_ipv4_text(auto a) {
    return std::format("{}.{}.{}.{}", a.octets[0], a.octets[1], a.octets[2], a.octets[3]);
};

bitfield VersionIhl {
    version : 4;
    ihl : 4 [[comment("header length in 32-bit words")]];
};

bitfield FlagsFragment {
    reserved : 1;
    dont_fragment : 1;
    more_fragments : 1;
    fragment_offset : 13;
};

bitfield TcpFlags {
    data_offset : 4 [[comment("header length in 32-bit words")]];
    reserved : 3;
    ns : 1;
    cwr : 1;
    ece : 1;
    urg : 1;
    ack : 1;
    psh : 1;
    rst : 1;
    syn : 1;
    fin : 1;
};

struct Tcp {
    u16 source_port;
    u16 destination_port;
    u32 sequence_number;
    u32 acknowledgment_number;
    TcpFlags flags;
    u16 window;
    u16 checksum [[format("hex"), comment("over pseudo header, header and data")]];
    u16 urgent_pointer;
    if (flags.data_offset > 5) u8 options[(flags.data_offset - 5) * 4];
    u8 payload[];
} [[color("27AE60")]];

struct Udp {
    u16 source_port;
    u16 destination_port;
    u16 length;
    u16 checksum [[format("hex")]];
    if (length > 8) u8 payload[std::math::min(length - 8, std::mem::size() + std::mem::base_address() - $)];
} [[color("8E44AD")]];

struct IPv4 {
    VersionIhl version_ihl;
    u8 dscp_ecn;
    u16 total_length;
    u16 identification;
    FlagsFragment flags_fragment;
    u8 ttl;
    IpProtocol protocol;
    u16 header_checksum [[format("hex"),
        checksum("internet", addressof(this), addressof(this) + version_ihl.ihl * 4)]];
    IPv4Address source;
    IPv4Address destination;
    if (version_ihl.ihl > 5) u8 options[(version_ihl.ihl - 5) * 4];
    u64 payload_size = total_length > version_ihl.ihl * 4 ? total_length - version_ihl.ihl * 4 : 0;
    payload_size = std::math::min(payload_size, std::mem::size() + std::mem::base_address() - $);
    match (protocol) {
        (IpProtocol::TCP): Tcp tcp [[size(payload_size)]];
        (IpProtocol::UDP): Udp udp [[size(payload_size)]];
        (_): u8 payload[payload_size];
    }
} [[color("2E86C1")]];

struct Ethernet {
    MacAddress destination;
    MacAddress source;
    EtherType ether_type;
    if (ether_type == EtherType::VLAN || ether_type == EtherType::QinQ) {
        u16 tag_control [[comment("PCP (3) / DEI (1) / VLAN id (12)")]];
        EtherType inner_type;
    }
    u16 effective_type = std::core::has_member(this, "inner_type") ? inner_type : ether_type;
    match (effective_type) {
        (EtherType::IPv4): IPv4 ipv4;
        (_): u8 payload[];
    }
} [[color("E67E22")]];
