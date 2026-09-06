.PHONY: all api agent ui bootstrap dev-cluster clean

GO := go
DOCKER := docker
TALOSCTL := talosctl
KUBECTL := kubectl

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

dev-cluster:
	$(TALOSCTL) cluster create --name nasos-dev \
		--image factory.talos.dev/$(shell $(TALOSCTL) image factory schematic render \
			--schematic bootstrap/schematic/nasos.yaml | tail -1) \
		--workers 0

clean:
	rm -rf bin/ ui/dist ui/node_modules
