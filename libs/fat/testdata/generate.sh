#!/bin/sh
# Regenerates the FAT test images (ADR-0036) with the reference tools:
# dosfstools (mkfs.fat) and mtools, run on Linux (WSL works; the packages
# can be unpacked without root: `apt download dosfstools mtools`, then
# `dpkg-deb -x` each). Every image holds the same tree; `sparse.py` stores
# only the non-zero parts, which the tests and xtask expand again.
#
#   fat12.img   1.44 MB floppy layout, no partition table ("superfloppy")
#   fat16.img   16 MiB disk, MBR, one FAT16 partition at 1 MiB (type 0x0e)
#   fat32.img   40 MiB disk, GPT, one basic data partition at 1 MiB
set -e
export LANG=C.UTF-8 LC_ALL=C.UTF-8
TOOLS=${TOOLS:-$HOME/fattools/root}
export PATH="$TOOLS/usr/sbin:$TOOLS/usr/bin:$PATH"
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
cd "$work"

printf 'hello from FAT\n' > hello.txt
printf 'long names work\n' > long.txt
printf 'deep\n' > deep.txt
printf 'สวัสดี\n' > thai.txt
: > empty.txt
head -c 12288 /dev/zero > pad1
head -c 4096 /dev/zero > pad2
python3 -c "import sys; sys.stdout.buffer.write(bytes(i % 251 for i in range(20000)))" > frag.bin

# fill IMAGE@@OFFSET
fill() {
  t="$1"
  mcopy -i "$t" hello.txt ::/HELLO.TXT
  mcopy -i "$t" long.txt "::/A long file name.txt"
  mcopy -i "$t" long.txt ::/long-file-name.txt
  mcopy -i "$t" thai.txt "::/ไฟล์ภาษาไทย.txt"
  mcopy -i "$t" empty.txt ::/empty.txt
  mmd -i "$t" ::/Docs
  mmd -i "$t" ::/Docs/notes
  mcopy -i "$t" deep.txt ::/Docs/notes/deep.txt
  # Fragmentation: frag.bin starts in the hole pad1 leaves.
  mcopy -i "$t" pad1 ::/pad1
  mcopy -i "$t" pad2 ::/pad2
  mdel -i "$t" ::/pad1
  mcopy -i "$t" frag.bin ::/frag.bin
}

rm -f fat12.img fat16.img fat32.img
mkfs.fat -C -F 12 -n OCEANS12 fat12.img 1440 > /dev/null
fill fat12.img

head -c $((16 * 1024 * 1024)) /dev/zero > fat16.img
printf 'label: dos\nstart=2048, type=e\n' | sfdisk -q fat16.img
mkfs.fat -F 16 -n OCEANS16 --offset 2048 fat16.img $(((16 * 1024 - 1024))) > /dev/null
fill fat16.img@@1M

head -c $((40 * 1024 * 1024)) /dev/zero > fat32.img
printf 'label: gpt\nstart=2048, size=75776, type=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7\n' | sfdisk -q fat32.img
mkfs.fat -F 32 -s 1 -n OCEANS32 --offset 2048 fat32.img $((75776 / 2)) > /dev/null
fill fat32.img@@1M

for image in fat12 fat16 fat32; do
  python3 "$here/sparse.py" pack "$image.img" "$here/$image.sparse"
done
rm -rf "$work"
