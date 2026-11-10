#pragma description Master boot record partition table with extended partitions

fn mbr_extended_index() {
    for (u8 i = 0, i < 4, i += 1) {
        u8 kind = mbr.partitions[i].type;
        if (kind == PartitionType::Extended || kind == PartitionType::Extended_LBA
                || kind == PartitionType::LinuxExtended)
            return i;
    }
    return 4;
};

MasterBootRecord mbr @ 0;
u8 extended_index = mbr_extended_index();
if (extended_index < 4) {
    u32 extended_lba = mbr.partitions[extended_index].first_lba;
    if (extended_lba != 0 && extended_lba * 512 + 512 <= std::mem::size())
        ExtendedBootRecord<extended_lba> logical_partitions @ extended_lba * 512;
}
