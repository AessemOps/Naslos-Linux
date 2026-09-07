.PHONY: all api agent ui bootstrap dev-cluster crds install uninstall clean

GO := go
DOCKER := docker
TALOSCTL := talosctl
KUBECTL := kubectl
HELM := helm

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

# Install Traefik CRDs (required before first Helm install)
crds:
	$(HELM) show crds traefik/traefik | $(KUBECTL) apply --server-side --force-conflicts -f -

# Install NasOS Helm chart
install: crds
	$(HELM) dependency update charts/nasos
	$(HELM) upgrade --install nasos charts/nasos -n nasos --create-namespace

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
