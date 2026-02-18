# Makefile for mirror management system
# This Makefile orchestrates the mirror discovery and processing pipeline

# Directory and file paths
SCRIPT_DIR := .
INPUT_DIR := input
OUTPUT_DIR := output
SOURCES_DIR := ../sources
INPUT_FILES := mirrors-alpine.txt mirrors-archlinux.txt mirrors-debian.html \
			   mirrors-fedora.html mirrors-openeuler.html mirrors-opensuse.html \
			   mirrors-ubuntu.html
INPUT_FILES_EXIST := $(wildcard $(addprefix $(INPUT_DIR)/, $(INPUT_FILES)))
YAML_FILES := $(wildcard $(SOURCES_DIR)/*.yaml)

# Directories to create
DIRS := $(INPUT_DIR) $(OUTPUT_DIR)

# Stage output files
STAGE1_OUT := $(OUTPUT_DIR)/official-mirrors.json
STAGE2_OUT := $(OUTPUT_DIR)/ls-mirrors.json
STAGE3_OUT := $(OUTPUT_DIR)/probe-mirrors.json $(OUTPUT_DIR)/noreach-mirrors.txt $(OUTPUT_DIR)/nocontent-mirrors.txt
STAGE4_OUT := $(SOURCES_DIR)/mirrors.json

# Python scripts with epkg execution
EPKG_RUN = epkg run
FETCH_SCRIPT := $(SCRIPT_DIR)/fetch_official_mirrors.py
LIST_SCRIPT := $(SCRIPT_DIR)/ls_mirrors.py
PROBE_SCRIPT := $(SCRIPT_DIR)/probe_dirs.py
MERGE_SCRIPT := $(SCRIPT_DIR)/merge_mirrors.py

# Default target - show help
.PHONY: help
help:
	@echo "Mirror Management System Makefile"
	@echo ""
	@echo "Available targets:"
	@echo "  init         - Create directories and setup epkg environment"
	@echo "  all          - Run complete pipeline (stages 1-4)"
	@echo "  stage1       - Fetch new mirrors from official sources"
	@echo "  stage2       - List directory contents from discovered mirrors"
	@echo "  stage3       - Probe unknown mirrors for directory structure"
	@echo "  stage4       - Merge all data into final configuration"
	@echo "  clean-cache  - Clean HTML and LFTP cache files"
	@echo "  clean        - Clean all generated files (except input files)"
	@echo "  help         - Show this help message"
	@echo ""
	@echo "Usage examples:"
	@echo "  make init         # Create directories and setup environment"
	@echo "  make all          # Run complete pipeline"
	@echo "  make stage1       # Only fetch new mirrors"
	@echo "  make stage2       # Only list directories"

# Initial setup: create directories and setup epkg environment
.PHONY: init
init: $(DIRS)
	@echo "Directories created: $(DIRS)"
	@echo "Setting up epkg environment and installing dependencies..."
	@if [ ! -d ".eenv" ]; then \
		epkg env create --root ".eenv" -c debian; \
	else \
		echo "epkg environment already exists at .eenv, skipping creation"; \
	fi
	epkg install python3-pip geoip-database chromium-driver lftp
	epkg run pip install -r requirements.txt
	@echo "Note: Input mirror lists will be downloaded automatically when stage1 runs"
	@echo "Initial setup complete"

# Create directories
$(DIRS):
	@mkdir -p $@

# Complete pipeline
.PHONY: all
all: $(STAGE4_OUT)

# Stage 1: Mirror Discovery
# Input: mirrors-*.html, mirrors-*.txt (downloaded from distribution websites)
# Output: output/official-mirrors.json
.PHONY: stage1 fetch
stage1 fetch: $(STAGE1_OUT)

$(STAGE1_OUT): $(FETCH_SCRIPT) $(INPUT_FILES_EXIST) $(DIRS)
	@echo "Stage 1: Fetching new mirrors from official sources..."
	cd $(SCRIPT_DIR) && $(EPKG_RUN) ./$(notdir $(FETCH_SCRIPT))
	@echo "Stage 1 complete: $(STAGE1_OUT) generated"

# Stage 2: Directory Listing
# Input: official-mirrors.json + previous ls-mirrors.json + sources/*.yaml
# Output: output/ls-mirrors.json
.PHONY: stage2 list
stage2 list: $(STAGE2_OUT)

$(STAGE2_OUT): $(LIST_SCRIPT) $(STAGE1_OUT) $(YAML_FILES) $(DIRS)
	@echo "Stage 2: Listing directory contents from discovered mirrors..."
	cd $(SCRIPT_DIR) && $(EPKG_RUN) ./$(notdir $(LIST_SCRIPT))
	@echo "Stage 2 complete: $(STAGE2_OUT) updated"


# Stage 3: Mirror Probing
# Input: ls-mirrors.json + sources/*.yaml
# Output: output/probe-mirrors.json + output/noreach-mirrors.txt + output/nocontent-mirrors.txt
.PHONY: stage3 probe
stage3 probe: $(OUTPUT_DIR)/probe-mirrors.json $(OUTPUT_DIR)/noreach-mirrors.txt $(OUTPUT_DIR)/nocontent-mirrors.txt

# Primary target: probe-mirrors.json generates all three files
$(OUTPUT_DIR)/probe-mirrors.json: $(PROBE_SCRIPT) $(STAGE2_OUT) $(YAML_FILES) $(DIRS)
	@echo "Stage 3: Probing unknown mirrors for directory structure..."
	cd $(SCRIPT_DIR) && $(EPKG_RUN) ./$(notdir $(PROBE_SCRIPT))
	@echo "Stage 3 complete: probe-mirrors.json generated"

# Secondary files are created by the same script
$(OUTPUT_DIR)/noreach-mirrors.txt $(OUTPUT_DIR)/nocontent-mirrors.txt: $(OUTPUT_DIR)/probe-mirrors.json
	@# These files are created as side effects of building probe-mirrors.json
	@if [ ! -f $(OUTPUT_DIR)/noreach-mirrors.txt ]; then \
		echo "Warning: $(OUTPUT_DIR)/noreach-mirrors.txt was not created by probe_dirs.py"; \
	fi
	@if [ ! -f $(OUTPUT_DIR)/nocontent-mirrors.txt ]; then \
		echo "Warning: $(OUTPUT_DIR)/nocontent-mirrors.txt was not created by probe_dirs.py"; \
	fi

# Stage 4: Data Merging
# Input: All JSON files + error files
# Output: sources/mirrors.json
.PHONY: stage4 merge
stage4 merge: $(STAGE4_OUT)

$(STAGE4_OUT): $(MERGE_SCRIPT) $(STAGE1_OUT) $(STAGE2_OUT) $(word 1, $(STAGE3_OUT)) \
			   $(YAML_FILES) $(DIRS)
	@echo "Stage 4: Merging all data into final configuration..."
	cd $(SCRIPT_DIR) && $(EPKG_RUN) ./$(notdir $(MERGE_SCRIPT))
	@echo "Stage 4 complete: $(STAGE4_OUT) generated"

# Clean targets
.PHONY: clean-cache clean
clean-cache:
	@echo "Cleaning HTML and LFTP cache files..."
	-rm -f html-cache/*.html
	-rm -f lftp-cache/*.lftp
	@echo "Cache cleaned"

clean:
	@echo "Cleaning all generated files..."
	-rm -f $(STAGE1_OUT)
	-rm -f $(STAGE2_OUT)
	-rm -f $(STAGE3_OUT)
	@echo "All generated files cleaned"

# Phony targets declaration
.PHONY: all stage1 stage2 stage3 stage4 fetch list probe merge help init
