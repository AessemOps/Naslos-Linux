.PHONY: all api agent ui images push-images \
        buddyctl buddy-receiver-image \
        bootstrap bootstrap-vm dev-cluster crds \
        install install-vm install-prod uninstall clean

GO := go
DOCKER := docker
TALOSCTL := talosctl
KUBECTL := kubectl
HELM := helm

VM_IP := 192.168.1.96
VM_CONFIG_DIR := bootstrap/vm
REGISTRY ?= 192.168.1.2:30095
IMAGE_TAG ?= 0.1.0

API_IMAGE := $(REGISTRY)/naslos-api:$(IMAGE_TAG)
AGENT_IMAGE := $(REGISTRY)/naslos-agent:$(IMAGE_TAG)
UI_IMAGE := $(REGISTRY)/naslos-ui:$(IMAGE_TAG)
OPENLDAP_IMAGE := $(REGISTRY)/naslos-openldap:$(IMAGE_TAG)
SAMBA_IMAGE := $(REGISTRY)/naslos-samba:$(IMAGE_TAG)
NFS_IMAGE := $(REGISTRY)/naslos-nfs:$(IMAGE_TAG)
TERMINAL_IMAGE := $(REGISTRY)/naslos-terminal:$(IMAGE_TAG)
BUDDY_RECEIVER_IMAGE := $(REGISTRY)/naslos-buddy-receiver:$(IMAGE_TAG)

# Extra flags passed through to helm upgrade (e.g. image registry overrides).
HELM_FLAGS :=

# File/dir names: accept both the post-rename names (charts/naslos,
# naslos-vm.yaml, naslos.yaml) and the pre-rename ones, so the repo works
# either way (see the rename note in docs/deployment.md).
CHART_DIR := charts/naslos
VM_PATCH := $(if $(wildcard $(VM_CONFIG_DIR)/naslos-vm.yaml),$(VM_CONFIG_DIR)/naslos-vm.yaml)
SCHEMATIC := $(if $(wildcard bootstrap/schematic/naslos.yaml),bootstrap/schematic/naslos.yaml)

all: api agent ui

api:
	cd api && $(GO) build -o ../bin/naslos-api ./cmd

agent:
	cd agent && $(GO) build -o ../bin/naslos-agent ./cmd

ui:
	cd ui && npm install && npm run build

# Buddy Backup client: the sender an operator runs (docs/buddy-backup.md).
buddyctl:
	cd api && $(GO) build -o ../bin/buddyctl ./cmd/buddyctl

# Build all Naslos container images locally.
images: api-image agent-image ui-image openldap-image samba-image nfs-image terminal-image buddy-receiver-image

api-image:
	$(DOCKER) build -t $(API_IMAGE) -f api/Dockerfile .

agent-image:
	$(DOCKER) build -t $(AGENT_IMAGE) -f agent/Dockerfile .

ui-image:
	$(DOCKER) build -t $(UI_IMAGE) -f ui/Dockerfile .

openldap-image:
	$(DOCKER) build -t $(OPENLDAP_IMAGE) -f openldap/image/Dockerfile openldap/image

samba-image:
	$(DOCKER) build -t $(SAMBA_IMAGE) -f samba/image/Dockerfile samba/image

nfs-image:
	$(DOCKER) build -t $(NFS_IMAGE) -f nfs/image/Dockerfile nfs/image

# The web terminal's exec target: a privileged shell container with the host
# mounted, because Talos has no shell of its own (spec FR-LOG-02).
terminal-image:
	$(DOCKER) build -t $(TERMINAL_IMAGE) -f terminal/image/Dockerfile terminal/image

# The standalone Buddy Backup receiver: a two-volume container that stores
# encrypted backups it cannot read (docs/buddy-backup.md).
buddy-receiver-image:
	$(DOCKER) build -t $(BUDDY_RECEIVER_IMAGE) --build-arg VERSION=$(IMAGE_TAG) -f api/Dockerfile.receiver .

# Push all Naslos container images to REGISTRY (requires docker login / insecure-registry config for HTTP registries).
push-images: images
	$(DOCKER) push $(API_IMAGE)
	$(DOCKER) push $(AGENT_IMAGE)
	$(DOCKER) push $(UI_IMAGE)
	$(DOCKER) push $(OPENLDAP_IMAGE)
	$(DOCKER) push $(SAMBA_IMAGE)
	$(DOCKER) push $(NFS_IMAGE)
	$(DOCKER) push $(TERMINAL_IMAGE)
	$(DOCKER) push $(BUDDY_RECEIVER_IMAGE)

# Print the digests of the images built for IMAGE_TAG, for pinning them in
# values (NAS-022: a tag is mutable, a digest is not).
image-digests:
	@for image in $(API_IMAGE) $(AGENT_IMAGE) $(UI_IMAGE) $(OPENLDAP_IMAGE) \
	              $(SAMBA_IMAGE) $(NFS_IMAGE) $(TERMINAL_IMAGE) $(BUDDY_RECEIVER_IMAGE); do \
		digest=$$($(DOCKER) inspect --format '{{index .RepoDigests 0}}' $$image 2>/dev/null); \
		if [ -n "$$digest" ]; then echo "$$image -> $$digest"; \
		else echo "$$image -> (not pushed yet: push first, a digest only exists remotely)"; fi; \
	done

bootstrap:
	$(TALOSCTL) image factory schematic bundle \
		--schematic $(SCHEMATIC) \
		--output bootstrap/naslos-installer.tar

# Generate a single-node Talos control-plane config for the VM and merge the Naslos patch.
# Outputs: bootstrap/vm/controlplane.yaml, bootstrap/vm/talosconfig
# NOTE: `talosctl gen config --force` regenerates ALL PKI. Only run this on a
# fresh node. Re-running it against an already-installed node orphans the
# existing talosconfig (bootstrap then fails with "certificate signed by
# unknown authority"). If the node is already installed, keep the existing
# talosconfig and just re-apply (see scripts/deploy-vm.sh fallback).
# NOTE 2: v1.14 `gen config` emits a stock UnattendedInstallConfig doc that
# is mutually exclusive with our machine.install block — it is stripped
# below so machine.install stays authoritative.
bootstrap-vm:
	@if [ -f $(VM_CONFIG_DIR)/talosconfig ] && [ -z "$(REGEN)" ]; then \
		echo "ERROR: $(VM_CONFIG_DIR)/talosconfig already exists."; \
		echo "Refusing to regenerate PKI (would orphan the installed node)."; \
		echo "To force regeneration on a FRESH node only: make bootstrap-vm REGEN=1"; \
		exit 1; \
	fi
	@mkdir -p $(VM_CONFIG_DIR)
	$(TALOSCTL) gen config naslos-vm \
		https://$(VM_IP):6443 \
		--output-types controlplane,talosconfig \
		--output $(VM_CONFIG_DIR) \
		--force \
		--with-docs=false \
		--with-examples=false \
		--config-patch @$(VM_PATCH)
	@echo "VM bootstrap config written to $(VM_CONFIG_DIR)/controlplane.yaml (UnattendedInstallConfig stripped, machine.install wins)"
	@python3 -c "import re,sys; p=sys.argv[1]; t=open(p).read(); d=re.split(r'(?m)^---\s*$$', t); open(p,'w').write('---'.join(x for x in d if 'kind: UnattendedInstallConfig' not in x))" $(VM_CONFIG_DIR)/controlplane.yaml
	@if grep -q "^kind: UnattendedInstallConfig" $(VM_CONFIG_DIR)/controlplane.yaml; then echo "ERROR: strip failed"; exit 1; fi
	@echo "talosconfig written to $(VM_CONFIG_DIR)/talosconfig"
	@echo "Next: boot the VM from the Naslos ISO and run:"
	@echo "  export TALOSCONFIG=$(VM_CONFIG_DIR)/talosconfig"
	@echo "  talosctl apply-config --insecure --nodes $(VM_IP) --file $(VM_CONFIG_DIR)/controlplane.yaml"
	@echo "  talosctl bootstrap --nodes $(VM_IP) --endpoints $(VM_IP)"

# Install Traefik CRDs (required before first Helm install)
crds:
	$(HELM) repo add traefik https://traefik.github.io/charts --force-update
	$(HELM) repo update traefik
	$(HELM) show crds traefik/traefik | $(KUBECTL) apply --server-side --force-conflicts -f -

# Install Naslos Helm chart
install: crds
	$(HELM) dependency update $(CHART_DIR)
	$(HELM) upgrade --install naslos $(CHART_DIR) -n naslos --create-namespace \
		--skip-crds

# Install Naslos on the single-node VM using the VM-specific values override.
install-vm: crds
	$(HELM) dependency update $(CHART_DIR)
	$(HELM) upgrade --install naslos $(CHART_DIR) -n naslos --create-namespace \
		-f $(CHART_DIR)/values.yaml \
		-f $(CHART_DIR)/values-vm.yaml \
		--skip-crds \
		$(HELM_FLAGS)

# Install the production posture on the single-node VM: Traefik on the node's
# 80/443 with Authelia forwardAuth (https://naslos.local). values-vm.yaml keeps
# the VM specifics and values-prod.yaml overrides its dev posture. `install-vm`
# stays the dev profile used by the Playwright suite, and is the rollback.
install-prod: crds
	$(HELM) dependency update $(CHART_DIR)
	$(HELM) upgrade --install naslos $(CHART_DIR) -n naslos --create-namespace \
		-f $(CHART_DIR)/values.yaml \
		-f $(CHART_DIR)/values-vm.yaml \
		-f $(CHART_DIR)/values-prod.yaml \
		--skip-crds \
		$(HELM_FLAGS)

# Uninstall Naslos
uninstall:
	$(HELM) uninstall naslos -n naslos

dev-cluster:
	$(TALOSCTL) cluster create --name naslos-dev \
		--image factory.talos.dev/$(shell $(TALOSCTL) image factory schematic render \
			--schematic $(SCHEMATIC) | tail -1) \
		--workers 0

clean:
	rm -rf bin/ ui/dist ui/node_modules bootstrap/*.tar *.iso
