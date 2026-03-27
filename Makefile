run:
	rm -f ~/.nerv/debug.log
	cargo run

setup:
	prek install --install-hooks

pc:
	prek --quiet run --all-files

install-skills:
	@mkdir -p ~/.nerv/skills
	@cp -n skills/*.md ~/.nerv/skills/ 2>/dev/null || true
	@echo "Skills installed to ~/.nerv/skills/"

bench:
	cargo bench --bench startup

install: install-skills
	cargo build --release
	@echo "Binary at target/release/nerv"
