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

CFLAGS   = -O2 -w -I$(ANGRYLION_CORE)
CXXFLAGS = -O2 -w -std=c++14 -I$(ANGRYLION_CORE)

driver: driver.o n64video.o parallel.o
	$(CXX) $^ -o $@ -pthread

n64video.o: $(ANGRYLION_CORE)/n64video.c
	$(CC) $(CFLAGS) -c $< -o $@
parallel.o: $(ANGRYLION_CORE)/parallel.cpp
	$(CXX) $(CXXFLAGS) -c $< -o $@
driver.o: driver.c
	$(CC) $(CFLAGS) -c $< -o $@

clean:
	rm -f driver *.o
