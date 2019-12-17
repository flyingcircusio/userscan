# rustup target add x86_64-unknown-linux-musl
# nix-shell -p lzo pkgconfig clang docutils --run make

TGT = target/x86_64-unknown-linux-musl/release
VERSION = $(shell cargo pkgid | sed 's/.*://')
PV = fc-userscan-$(VERSION)

# create a tarball containing a static binary and the man page
release: dist/$(PV)/fc-userscan.1
	cargo test
	cargo build --release --target x86_64-unknown-linux-musl
	mkdir -p dist/$(PV)
	install -m 0755 $(TGT)/fc-userscan dist/$(PV)
	cd dist && tar czf $(PV).tar.gz $(PV)

dist/$(PV)/fc-userscan.1: userscan.1.rst
	mkdir -p dist/$(PV)
	sed 's/@version@/$(VERSION)/' userscan.1.rst > dist/userscan.1.rst
	rst2man.py dist/userscan.1.rst > dist/$(PV)/fc-userscan.1
	rm dist/userscan.1.rst

clean:
	rm -rf dist

cleanall: clean
	cargo clean

.PHONY: release clean cleanall
