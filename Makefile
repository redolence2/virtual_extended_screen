# Remote Extended Screen — Top-level Makefile
# All build artifacts stay within the project directory.
# No system-wide installs. Tools downloaded to tools/bin/.

PROJECT_ROOT := $(shell pwd)
TOOLS_BIN    := $(PROJECT_ROOT)/tools/bin
MAC_DIR      := $(PROJECT_ROOT)/mac-host
UBUNTU_DIR   := $(PROJECT_ROOT)/ubuntu-client
PROTO_DIR    := $(PROJECT_ROOT)/proto

# Use local tools
export PATH := $(TOOLS_BIN):$(PATH)

.PHONY: all clean proto mac-build mac-run ubuntu-build ubuntu-run setup help

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}'

all: proto mac-build ## Build everything (proto + mac host)

# --- Setup ---
setup: ## Download local tools (protoc, protoc-gen-swift)
	@echo "==> Setting up local tools..."
	@bash $(PROJECT_ROOT)/tools/generate_proto.sh
	@echo "==> Setup complete. Tools in: $(TOOLS_BIN)"

# --- Protobuf ---
proto: ## Generate protobuf code for Swift and Rust
	@bash $(PROJECT_ROOT)/tools/generate_proto.sh

# --- Mac Host ---
mac-build: ## Build the macOS host (Swift)
	@echo "==> Building mac-host..."
	cd $(MAC_DIR) && swift build 2>&1

mac-build-release: ## Build the macOS host (release)
	@echo "==> Building mac-host (release)..."
	cd $(MAC_DIR) && swift build -c release 2>&1

mac-run: mac-build ## Build and run the macOS host (default 1920x1080@60)
	@echo "==> Running mac-host..."
	cd $(MAC_DIR) && swift run remote-display-host 1920 1080 60

mac-run-4k: mac-build ## Build and run the macOS host (4K: 3840x2160@60)
	@echo "==> Running mac-host (4K)..."
	cd $(MAC_DIR) && swift run remote-display-host 3840 2160 60

# --- Ubuntu Client ---
ubuntu-build: ## Build the Ubuntu client (Rust) — run on Ubuntu
	@echo "==> Building ubuntu-client..."
	cd $(UBUNTU_DIR) && cargo build 2>&1

ubuntu-build-release: ## Build the Ubuntu client (release)
	@echo "==> Building ubuntu-client (release)..."
	cd $(UBUNTU_DIR) && cargo build --release 2>&1

ubuntu-run: ubuntu-build ## Build and run the Ubuntu client
	@echo "==> Running ubuntu-client..."
	cd $(UBUNTU_DIR) && cargo run 2>&1

# --- Clean ---
clean: ## Remove all build artifacts (stays within project)
	@echo "==> Cleaning..."
	cd $(MAC_DIR) && swift package clean 2>/dev/null || true
	rm -rf $(MAC_DIR)/.build
	cd $(UBUNTU_DIR) && cargo clean 2>/dev/null || true
	rm -rf $(UBUNTU_DIR)/target
	@echo "==> Clean complete"

clean-tools: ## Remove downloaded tools
	@echo "==> Removing tools..."
	rm -rf $(TOOLS_BIN) $(PROJECT_ROOT)/tools/include $(PROJECT_ROOT)/tools/lib
	@echo "==> Tools removed"

clean-all: clean clean-tools ## Remove everything (build + tools)

# --- Info ---
info: ## Show environment info
	@echo "Project root: $(PROJECT_ROOT)"
	@echo "Tools bin:    $(TOOLS_BIN)"
	@echo "Swift:        $$(swift --version 2>/dev/null | head -1 || echo 'not found')"
	@echo "Cargo:        $$(cargo --version 2>/dev/null || echo 'not found')"
	@echo "Protoc:       $$($(TOOLS_BIN)/protoc --version 2>/dev/null || echo 'not installed — run make setup')"
	@echo "OS build:     $$(sw_vers -buildVersion 2>/dev/null || echo 'N/A')"
