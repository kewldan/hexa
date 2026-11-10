#pragma description libpcap capture file: header and packet records decoded as Ethernet/IPv4/TCP/UDP

enum LinkType : u32 {
    Null = 0,
    Ethernet = 1,
    IEEE802_5 = 6,
    PPP = 9,
    Raw = 101,
    IEEE802_11 = 105,
    LinuxSLL = 113,
    IEEE802_11_Radiotap = 127,
    LinuxSLL2 = 276
};

struct FileHeader {
    u32 magic [[format("hex"), comment("A1B2C3D4 = microseconds, A1B23C4D = nanoseconds")]];
    u16 version_major;
    u16 version_minor;
    s32 this_zone;
    u32 sigfigs;
    u32 snap_length;
    LinkType network;
} [[color("2E86C1")]];

struct Record {
    u32 timestamp_seconds [[format("unix_time_text")]];
    u32 timestamp_fraction [[comment("micro- or nanoseconds")]];
    u32 captured_length;
    u32 original_length;
    u64 available = std::math::min(captured_length, std::mem::size() - $);
    if (pcap_link == LinkType::Ethernet && available >= 14) be Ethernet ethernet [[size(available)]];
    else if (pcap_link == LinkType::Raw && available >= 20) be IPv4 ipv4 [[size(available)]];
    else u8 data[available];
} [[color("95A5A6")]];

struct PcapFile {
    FileHeader header;
    Record records[];
};

u32 pcap_magic = std::mem::read_unsigned(0, 4);
bool pcap_big = pcap_magic == 0xD4C3B2A1 || pcap_magic == 0x4D3CB2A1;
assert(pcap_big || pcap_magic == 0xA1B2C3D4 || pcap_magic == 0xA1B23C4D,
    "not a pcap file: bad magic");
u32 pcap_link = std::mem::read_unsigned(20, 4, pcap_big ? "big" : "little");
if (pcap_big) be PcapFile pcap @ 0;
else le PcapFile pcap @ 0;
