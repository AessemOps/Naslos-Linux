.PHONY: all api agent ui bootstrap bootstrap-vm dev-cluster crds install install-vm uninstall clean

GO := go
DOCKER := docker
TALOSCTL := talosctl
KUBECTL := kubectl
HELM := helm

VM_IP := 192.168.1.96
VM_CONFIG_DIR := bootstrap/vm

all: api agent ui

api:
	cd api && $(GO) build -o ../bin/nasos-api ./cmd

agent:
	cd agent && $(GO) build -o ../bin/nasos-agent ./cmd

ui:
	cd ui && npm install && npm run build

bootstrap:
	$(TALOSCTL) image factory schematic bundle \
		--schematic bootstrap/schematic/nasos.yaml \
		--output bootstrap/nasos-installer.tar

# Generate a single-node Talos control-plane config for the VM and merge the NasOS patch.
# Outputs: bootstrap/vm/controlplane.yaml, bootstrap/vm/talosconfig
bootstrap-vm:
	@mkdir -p $(VM_CONFIG_DIR)
	$(TALOSCTL) gen config nasos-vm \
		https://$(VM_IP):6443 \
		--output-types controlplane,talosconfig \
		--output $(VM_CONFIG_DIR) \
		--force
	$(TALOSCTL) machineconfig patch $(VM_CONFIG_DIR)/controlplane.yaml \
		--patch $(VM_CONFIG_DIR)/nasos-vm.yaml \
		--output $(VM_CONFIG_DIR)/controlplane.yaml
	@echo "VM bootstrap config written to $(VM_CONFIG_DIR)/controlplane.yaml"
	@echo "talosconfig written to $(VM_CONFIG_DIR)/talosconfig"
	@echo "Next: boot the VM from the NasOS ISO and run:"
	@echo "  talosctl apply-config --insecure --nodes $(VM_IP) --file $(VM_CONFIG_DIR)/controlplane.yaml"
	@echo "  talosctl bootstrap --nodes $(VM_IP) --endpoints $(VM_IP)"

# Install Traefik CRDs (required before first Helm install)
crds:
	$(HELM) show crds traefik/traefik | $(KUBECTL) apply --server-side --force-conflicts -f -

# Install NasOS Helm chart
install: crds
	$(HELM) dependency update charts/nasos
	$(HELM) upgrade --install nasos charts/nasos -n nasos --create-namespace

# Install NasOS on the single-node VM using the VM-specific values override.
install-vm: crds
	$(HELM) dependency update charts/nasos
	$(HELM) upgrade --install nasos charts/nasos -n nasos --create-namespace \
		-f charts/nasos/values.yaml \
		-f charts/nasos/values-vm.yaml

# Uninstall NasOS
uninstall:
	$(HELM) uninstall nasos -n nasos

dev-cluster:
	$(TALOSCTL) cluster create --name nasos-dev \
		--image factory.talos.dev/$(shell $(TALOSCTL) image factory schematic render \
			--schematic bootstrap/schematic/nasos.yaml | tail -1) \
		--workers 0

clean:
	rm -rf bin/ ui/dist ui/node_modules bootstrap/*.tar *.iso
