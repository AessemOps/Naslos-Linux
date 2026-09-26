.PHONY: all api agent ui images push-images \
        buddyctl buddy-receiver-image \
        bootstrap bootstrap-vm dev-cluster crds \
        cert-manager cert-manager-webhook-ovh \
        install install-vm uninstall clean

GO := go
DOCKER := docker
TALOSCTL := talosctl
KUBECTL := kubectl
HELM := helm

VM_IP := 192.168.1.96
VM_CONFIG_DIR := bootstrap/vm
REGISTRY ?= 192.168.1.2:30095
IMAGE_TAG ?= 0.1.0

# cert-manager + the OVH DNS-01 webhook (FR-APP-13/FR-DNS-05). OVH is not a
# cert-manager built-in solver, so it needs aureq/cert-manager-webhook-ovh. Both
# install into their own `cert-manager` namespace to stay outside the `naslos`
# default-deny network policies; the naslos chart grants the webhook read access
# to the domain credential Secrets in the apps namespace.
CERT_MANAGER_CHART_VERSION ?= v1.18.2
OVH_WEBHOOK_CHART_VERSION ?= 0.9.17
OVH_WEBHOOK_GROUP ?= ovh.naslos.local
CERT_MANAGER_NAMESPACE ?= cert-manager

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

# Authelia's pod template never references our ConfigMap, so editing
# templates/authelia-config.yaml alone would not roll its DaemonSet (the running
# pod keeps the old portal path and access rules). Pass a checksum as a pod
# annotation so the pod template changes and Helm restarts it. Defined after
# CHART_DIR: with := the $(shell) runs immediately, so an earlier definition
# would hash an empty path and the flag would silently expand to nothing.
AUTHELIA_CONFIG_SHA := $(shell sha256sum $(CHART_DIR)/templates/authelia-config.yaml 2>/dev/null | cut -c1-64)
AUTHELIA_CONFIG_FLAG := $(if $(AUTHELIA_CONFIG_SHA),--set authelia.pod.annotations.checksum-config=$(AUTHELIA_CONFIG_SHA))

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
	@# AUDIT-M4: splice the pinned Cilium manifest into the machine-config patch
	@# as a KubeInlineManifestConfig before generating, so a fresh install ships
	@# Cilium (flannel disabled, kube-proxy replaced) with no manual steps.
	@scripts/render-cilium.sh
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

# Install Traefik and cert-manager CRDs (required before first Helm install).
# cert-manager itself is optional (apps.certManager.enabled): the SSL page gates
# on the CRDs being present, and an unset cert-manager leaves the rest working.
crds:
	$(HELM) repo add traefik https://traefik.github.io/charts --force-update
	$(HELM) repo update traefik
	$(HELM) show crds traefik/traefik | $(KUBECTL) apply --server-side --force-conflicts -f -
	$(HELM) repo add jetstack https://charts.jetstack.io --force-update
	$(HELM) repo update jetstack
	# cert-manager keeps its CRDs in templates/crds.yaml (not crds/), so
	# `helm show crds` is empty; render just that template with the CRDs enabled.
	$(HELM) template cert-manager jetstack/cert-manager --version v1.18.2 \
		--show-only templates/crds.yaml --set crds.enabled=true \
		| $(KUBECTL) apply --server-side --force-conflicts -f -

# Install cert-manager and the OVH DNS-01 webhook, each as its own release in
# the `cert-manager` namespace. CRDs come from `make crds` (crds.enabled=false
# avoids a second, conflicting CRD install). The webhook's group name must match
# the `webhook.groupName` in api/internal/providers/builtin/ovh.yaml.
cert-manager:
	$(HELM) repo add jetstack https://charts.jetstack.io --force-update
	$(HELM) repo update jetstack
	$(HELM) upgrade --install cert-manager jetstack/cert-manager \
		-n $(CERT_MANAGER_NAMESPACE) --create-namespace \
		--version $(CERT_MANAGER_CHART_VERSION) \
		--set crds.enabled=false \
		--force-conflicts

cert-manager-webhook-ovh:
	$(HELM) repo add cert-manager-webhook-ovh https://aureq.github.io/cert-manager-webhook-ovh --force-update
	$(HELM) repo update cert-manager-webhook-ovh
	$(HELM) upgrade --install cert-manager-webhook-ovh \
		cert-manager-webhook-ovh/cert-manager-webhook-ovh \
		-n $(CERT_MANAGER_NAMESPACE) \
		--version $(OVH_WEBHOOK_CHART_VERSION) \
		--set groupName=$(OVH_WEBHOOK_GROUP) \
		--set certManager.namespace=$(CERT_MANAGER_NAMESPACE) \
		--set certManager.serviceAccountName=cert-manager \
		--force-conflicts

# Install Naslos Helm chart.
# --force-conflicts: Helm 4 applies server-side, and the namespaces are
# pre-created and PSA-labelled by deploy-vm.sh with kubectl; without this the
# install aborts with "conflict with kubectl-label ... pod-security.../enforce".
install: crds
	$(HELM) dependency update $(CHART_DIR)
	$(HELM) upgrade --install naslos $(CHART_DIR) -n naslos --create-namespace \
		--skip-crds \
		--force-conflicts \
		$(AUTHELIA_CONFIG_FLAG)

# Install Naslos on the single-node VM: Traefik on the node's 80/443 with
# Authelia forwardAuth (https://naslos.local), and every route authenticated.
# values-vm.yaml is the only profile; there is no unauthenticated posture to
# switch to or roll back into. cert-manager and the OVH webhook install first so
# a Domain's Issuer can be reconciled immediately.
install-vm: crds cert-manager cert-manager-webhook-ovh
	$(HELM) dependency update $(CHART_DIR)
	$(HELM) upgrade --install naslos $(CHART_DIR) -n naslos --create-namespace \
		-f $(CHART_DIR)/values.yaml \
		-f $(CHART_DIR)/values-vm.yaml \
		--skip-crds \
		--force-conflicts \
		$(AUTHELIA_CONFIG_FLAG) \
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
