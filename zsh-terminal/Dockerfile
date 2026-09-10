# Naslos Zsh Web Terminal
# Provides a zsh shell in a container with host mounts for NAS management.
# This is the user-facing shell — Talos itself has no shell by design.

FROM alpine:3.20

RUN apk add --no-cache \
    zsh \
    zsh-autosuggestions \
    zsh-syntax-highlighting \
    bash \
    curl \
    wget \
    jq \
    vim \
    nano \
    htop \
    iotop \
    ncdu \
    tmux \
    openssh-client \
    ca-certificates \
    && rm -rf /var/cache/apk/*

# Install kubectl
RUN curl -sLO "https://dl.k8s.io/release/$(curl -sL https://dl.k8s.io/release/stable.txt)/bin/linux/amd64/kubectl" \
    && install -o root -g root -m 0755 kubectl /usr/local/bin/kubectl

# Install talosctl
RUN curl -sL https://github.com/siderolabs/talos/releases/latest/download/talosctl-linux-amd64 -o /usr/local/bin/talosctl \
    && chmod +x /usr/local/bin/talosctl

# Install zfs tools (for pool inspection)
RUN apk add --no-cache zfs zfs-utils

# Configure zsh
RUN echo 'export PS1="%n@nasos:%~%# "' >> /etc/zsh/zshrc \
    && echo 'setopt AUTO_CD' >> /etc/zsh/zshrc \
    && echo 'setopt CORRECT' >> /etc/zsh/zshrc \
    && echo 'source /usr/share/zsh/plugins/zsh-autosuggestions/zsh-autosuggestions.zsh 2>/dev/null' >> /etc/zsh/zshrc \
    && echo 'source /usr/share/zsh/plugins/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh 2>/dev/null' >> /etc/zsh/zshrc

# Welcome message
RUN echo 'echo "Welcome to NasOS Terminal — type help for commands"' >> /etc/zsh/zshrc

WORKDIR /root
SHELL ["/bin/zsh", "-c"]
CMD ["/bin/zsh"]
