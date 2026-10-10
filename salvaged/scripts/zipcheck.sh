set -e
for pair in "Super Mario 64 (USA).zip:eeprom-4k/Super Mario 64.z64" \
            "Star Wars - Rogue Squadron (USA).zip:eeprom-4k/Star Wars - Rogue Squadron.z64" \
            "Perfect Dark (USA).zip:eeprom-16k/Perfect Dark.z64"; do
  zipf="${pair%%:*}"; z64="${pair##*:}"
  a=$(unzip -p "$HOME/Emulation/roms/n64/$zipf" | sha256sum | cut -d' ' -f1)
  b=$(sha256sum "tests/roms/external/commercial/$z64" | cut -d' ' -f1)
  if [ "$a" = "$b" ]; then echo "MATCH  $zipf"; else echo "DIFFER $zipf ($a vs $b)"; fi
done
