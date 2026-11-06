# The SFTP account's throwaway test image (ADR-0022, S15, U14): the builder
# image both Dockerfiles pin, with Debian 13's openssh-server, which brings
# its client and sftp-server, from Debian's signed archive. Only the slow
# test `slow_sftp_backups_account` builds and runs it, never on a host; a
# test holds its FROM equal to the images' builder.
FROM rust:1.99.0-slim-trixie@sha256:24e632c09342c20abf8312cf4f61430a911c01ed3a5e4c02b87292b1c39c5273
RUN apt-get update \
    && apt-get install --yes --no-install-recommends openssh-server \
    && rm -rf /var/lib/apt/lists/*
# Host keys, then sshd as PID 1, so a HUP to PID 1 reloads it.
CMD ["sh", "-c", "ssh-keygen -A && mkdir -p /run/sshd && exec /usr/sbin/sshd -D -e"]
