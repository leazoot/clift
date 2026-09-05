# Throwaway OpenSSH server used by Clift's integration tests.
#
# The specification requires the transport layer to be exercised against a real SFTP
# server rather than a mock, and 23.4 forbids passing a mocked transport off as
# an end-to-end test. Ubuntu 24.04 is used because it matches the project's
# reference hosts, so sftp-server lives at the same path here as there.
FROM ubuntu:24.04

RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        openssh-server \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir -p /run/sshd \
 # Host keys are generated per container by the entrypoint. Baking them into
 # the image would give every fixture the same identity, which is exactly the
 # situation host key verification exists to catch.
 && rm -f /etc/ssh/ssh_host_* \
 # uid 1000 belongs to the stock `ubuntu` account; the tests want a plain,
 # non-root account of their own so that a permission bug cannot be masked by
 # running as root.
 && userdel --remove ubuntu \
 && useradd --create-home --shell /bin/bash --uid 1000 dev \
 # The distribution config defines Subsystem itself; sshd refuses to start if a
 # drop-in defines it a second time.
 && sed -i 's|^Subsystem|#Subsystem|' /etc/ssh/sshd_config

COPY sshd.conf /etc/ssh/sshd_config.d/clift.conf
COPY entrypoint.sh /usr/local/bin/clift-fixture-entrypoint

RUN chmod 0644 /etc/ssh/sshd_config.d/clift.conf \
 && chmod 0755 /usr/local/bin/clift-fixture-entrypoint

EXPOSE 22
ENTRYPOINT ["/usr/local/bin/clift-fixture-entrypoint"]
