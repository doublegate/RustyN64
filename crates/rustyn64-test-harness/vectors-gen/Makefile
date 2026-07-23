# Build the standalone Angrylion golden-vector generator.
#
# Angrylion-rdp-plus is the non-commercial MAME-licensed study oracle — it is
# NOT vendored here. Fetch it yourself (into the gitignored ref-proj tree) and
# point ANGRYLION_CORE at its `src/core`. Only this driver (our own MIT code) and
# the emitted `.rvec` vectors (Angrylion's *output*, freely committable) live in
# the tree; no Angrylion source is committed. See README.md.
#
#   git -C <repo>/ref-proj/parallel-rdp submodule update --init --depth 1 angrylion-rdp-plus
#   make ANGRYLION_CORE=<repo>/ref-proj/parallel-rdp/angrylion-rdp-plus/src/core
#   ./driver <repo>/crates/rustyn64-test-harness/tests/vectors

ANGRYLION_CORE ?= ../../../ref-proj/parallel-rdp/angrylion-rdp-plus/src/core

# `-fno-strict-aliasing` is required on every TU: both this driver and Angrylion
# type-pun the RDRAM byte array through u32/u16 pointers (the hardware's memory
# layout), which is UB under the default strict-aliasing rules at -O2.
COMMON = -O2 -fno-strict-aliasing -I$(ANGRYLION_CORE)
# Our driver builds with warnings on; the vendored Angrylion sources are noisy
# old C we don't own, so they alone are silenced (`-w`).
DRIVER_CFLAGS    = $(COMMON) -Wall
ANGRYLION_CFLAGS = $(COMMON) -w
ANGRYLION_CXXFLAGS = $(COMMON) -w -std=c++14

driver: driver.o n64video.o parallel.o
	$(CXX) $^ -o $@ -pthread

n64video.o: $(ANGRYLION_CORE)/n64video.c
	$(CC) $(ANGRYLION_CFLAGS) -c $< -o $@
parallel.o: $(ANGRYLION_CORE)/parallel.cpp
	$(CXX) $(ANGRYLION_CXXFLAGS) -c $< -o $@
driver.o: driver.c
	$(CC) $(DRIVER_CFLAGS) -c $< -o $@

clean:
	rm -f driver *.o
