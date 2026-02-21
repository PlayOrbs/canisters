FEATURES ?=


.PHONY: all
all: build


.PHONY: build
.SILENT: build
build:
	@echo "Building for FEATURES $(FEATURES)..."
	./build.sh "$(FEATURES)"


# Deploy dev
.PHONY: deploy-dev
.SILENT: deploy-dev
deploy-dev:
	$(MAKE) build
	@echo "Deploying orbs_backend (dev) to icp..."
	dfx deploy --network=ic orbs_backend


# Deploy prod
.PHONY: deploy-prod
.SILENT: deploy-prod
deploy-prod:
	$(MAKE) build 
	@echo "Deploying orbs_backend (prod) to icp..."
	dfx deploy --network=ic orbs_backend_prod

# Default deploy is to local network
.PHONY: deploy
.SILENT: deploy
deploy: deploy-local


# Local deploy
.PHONY: deploy-local
.SILENT: deploy-local
deploy-local:
	$(MAKE) build
	@echo "Deploying orbs_backend to local network..."
	dfx deploy


# Check ICP balance on mainnet
.PHONY: check_icp_balance
.SILENT: check_icp_balance
check_icp_balance:
	@echo "Checking ICP balance on mainnet..."
	@dfx ledger --network=ic balance


# Shorthand for check_icp_balance
.PHONY: balance
.SILENT: balance
balance: check_icp_balance


# Delete all build artifacts
.PHONY: clean
.SILENT: clean
clean:
	rm -rf .dfx
	rm -rf dist
	rm -rf node_modules
	rm -rf src/declarations
	rm -f .env
	cargo clean
