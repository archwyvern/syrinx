# The fdlibm reference build. `make golden` regenerates test/fixtures/math-golden.json; `make test`
# at the repository root does NOT need this (it reads the committed fixture), so a C++ compiler is
# only required to regenerate it.
#
# -ffp-contract=off is the one flag that matters: with contraction on, a*b+c may become a single
# fused rounding and the reference would compute something the JavaScript port cannot.

CXX ?= clang++
CXXFLAGS ?= -std=c++17 -O2 -ffp-contract=off -fno-fast-math -fno-builtin -Wall -Istubs

FIXTURE := ../../test/fixtures/math-golden.json

.PHONY: golden clean

golden: build/golden
	build/golden > $(FIXTURE)
	@echo wrote $(FIXTURE)

build/golden: golden.cc ieee754.cc sweep.h stubs/src/base/*.h
	mkdir -p build
	$(CXX) $(CXXFLAGS) -o $@ ieee754.cc golden.cc

clean:
	rm -rf build
