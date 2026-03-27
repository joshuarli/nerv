NAME       := nerv
TARGET     := $(shell rustc -vV | awk '/^host:/ {print $$2}')

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

pc:
	prek --quiet run --all-files

install-skills:
	@mkdir -p ~/.nerv/skills
	@cp -n skills/*.md ~/.nerv/skills/ 2>/dev/null || true
	@echo "Skills installed to ~/.nerv/skills/"

bench:
	cargo bench --bench startup

install: release
	cp target/$(TARGET)/release/$(NAME) ~/usr/bin/$(NAME)
	codesign -fs - ~/usr/bin/$(NAME)
