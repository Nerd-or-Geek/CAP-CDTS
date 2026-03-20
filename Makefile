# CAP-CDTS Makefile
#
# Primary goal: build a *staged* binary that can be swapped in by the running backend.
# - `make` / `make build` => builds `bin/cap-cdts-backend.new`
# - `make promote`        => moves `.new` into place as `bin/cap-cdts-backend`
#
# NOTE: For Raspberry Pi deployment, run the systemd service from `bin/cap-cdts-backend`.

APP := cap-cdts-backend
BIN_DIR := bin
TARGET_BIN := target/release/$(APP)

EXE :=
ifeq ($(OS),Windows_NT)
EXE := .exe
endif

LIVE_BIN := $(BIN_DIR)/$(APP)$(EXE)
NEW_BIN := $(BIN_DIR)/$(APP).new$(EXE)
TARGET := $(TARGET_BIN)$(EXE)

.PHONY: build promote clean run

build:
	cargo build --release
	@mkdir -p "$(BIN_DIR)"
	@# Prefer `install` when available so permissions are correct on Linux.
	@if command -v install >/dev/null 2>&1; then \
		install -m 755 "$(TARGET)" "$(NEW_BIN)"; \
	else \
		cp "$(TARGET)" "$(NEW_BIN)"; \
	fi
	@# Optional size reduction (ignore errors if strip isn't available).
	@if command -v strip >/dev/null 2>&1; then strip "$(NEW_BIN)" || true; fi
	@echo "Built: $(NEW_BIN)"

promote:
	@mkdir -p "$(BIN_DIR)"
	@if [ ! -f "$(NEW_BIN)" ]; then echo "Missing $(NEW_BIN) (run 'make build' first)."; exit 2; fi
	@mv -f "$(NEW_BIN)" "$(LIVE_BIN)"
	@echo "Promoted: $(LIVE_BIN)"

run:
	cargo run

clean:
	cargo clean
	@rm -rf "$(BIN_DIR)"
