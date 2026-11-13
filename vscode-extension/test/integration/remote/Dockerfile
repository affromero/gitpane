FROM ubuntu:24.04
RUN apt-get update && apt-get install -y --no-install-recommends openssh-server git ca-certificates curl procps \
    && rm -rf /var/lib/apt/lists/* && useradd -m -s /bin/bash gitpanetest \
    && passwd -d gitpanetest && mkdir -p /run/sshd
CMD ["/usr/sbin/sshd", "-D", "-e", "-f", "/keys/sshd_config"]
