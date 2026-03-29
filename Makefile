NAME       := nerv
TARGET     := $(shell rustc -vV | awk '/^host:/ {print $$2}')
LLVM_PROFDATA := $(shell xcrun -f llvm-profdata 2>/dev/null)
PGO_DIR    := $(CURDIR)/target/pgo-profiles
PGO_MERGED := $(PGO_DIR)/merged.profdata

.PHONY: run setup release pgo-profile release-pgo bench-pgo bench test-ci pc install-skills install-prompts install bump-version

run:
	rm -f ~/.nerv/debug.log
	cargo run

setup:
	prek install --install-hooks

release:
	cargo clean -p $(NAME) --release --target $(TARGET)
	RUSTFLAGS="-Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort" \
	cargo build --release \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target $(TARGET)

# Collect PGO profiles from benchmarks — only re-run when hot paths change.
# No build-std or -Cpanic=immediate-abort here: the profiler runtime needs unwinding.
pgo-profile:
	rm -rf $(PGO_DIR) && mkdir -p $(PGO_DIR)
	RUSTFLAGS="-Cprofile-generate=$(PGO_DIR)" PGO_PROFILE=1 \
	cargo bench --bench chat_writer --bench highlight --bench index --bench json_encoding --bench tools
	$(LLVM_PROFDATA) merge -o $(PGO_MERGED) $(PGO_DIR)

# PGO-optimized release: uses gathered profiles + all aggressive flags.
release-pgo: $(PGO_MERGED)
	cargo clean -p $(NAME) --release --target $(TARGET)
	RUSTFLAGS="-Cprofile-use=$(PGO_MERGED) -Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort" \
	cargo build --release \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target $(TARGET)

# Benchmark regular release vs PGO. Requires: critcmp (cargo install critcmp)
bench-pgo: $(PGO_MERGED)
	cargo bench -- --save-baseline regular 2>/dev/null
	RUSTFLAGS="-Cprofile-use=$(PGO_MERGED)" \
	cargo bench -- --save-baseline pgo 2>/dev/null
	critcmp regular pgo

$(PGO_MERGED):
	$(MAKE) pgo-profile

pc:
	prek --quiet run --all-files

test-ci:
	@OUT=$$(cargo test --quiet --release -- --test-threads=32 2>&1) || { echo "$$OUT"; exit 1; }

install-skills:
	@mkdir -p ~/.nerv/skills
	@cp -n skills/*.md ~/.nerv/skills/ 2>/dev/null || true
	@echo "Skills installed to ~/.nerv/skills/"

install-prompts:
	@mkdir -p ~/.nerv/prompts
	@cp prompts/*.md ~/.nerv/prompts/ 2>/dev/null || true
	@echo "Prompts installed to ~/.nerv/prompts/"

bench:
	cargo bench --bench index --bench tools --bench json_encoding --bench chat_writer --bench highlight

install: release-pgo install-skills install-prompts
	cp target/$(TARGET)/release/$(NAME) ~/usr/bin/$(NAME)
	codesign -fs - ~/usr/bin/$(NAME)

# Usage: make bump-version [V=x.y.z]
# Without V, increments the patch version.
bump-version:
ifndef V
	$(eval OLD := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml))
	$(eval V := $(shell echo "$(OLD)" | awk -F. '{printf "%d.%d.%d", $$1, $$2, $$3+1}'))
endif
	sed -i '' 's/^version = ".*"/version = "$(V)"/' Cargo.toml
	cargo check --quiet 2>/dev/null
	git add Cargo.toml Cargo.lock
	git commit -m "bump version to $(V)"
	git tag "release/$(V)"

