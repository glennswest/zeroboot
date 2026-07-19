#!/bin/bash
# bootstrap-cluster.sh — stand up a single-node rustkube cluster on a running
# stormcos node (the installer's cluster-bootstrap phase, our openshift-install
# bootstrap analog). Follows rustkube's own masters/ deployment: systemd units +
# env files + PKI from openssl, single fastetcd member.
#
# Proven end to end 2026-07-19: node registers and goes Ready
#   (v1.32.0-rustkube+0.7.7), fastetcd + apiserver + controller-manager +
#   scheduler + kubelet all active.
#
# DEV MODE CAVEAT: this runs the control plane over plain HTTP with
# --anonymous-auth, because this rustkube-node kubelet build has no TLS/CA/
# kubeconfig options and can't join a TLS apiserver (rustkube-node#19). The PKI
# is generated and the control plane serves TLS fine on its own; only kubelet
# forces the HTTP fallback. Switch back to TLS once #19 lands.
#
# Usage: NODE_IP=192.168.8.66 NODE_NAME=stormcos-boot.g8.lo \
#        BIN_DIR=~/projects  ./bootstrap-cluster.sh
# Binaries are the static-musl builds of rustkube, rustkube-node, fastetcd.

set -euo pipefail

NODE_IP="${NODE_IP:?set NODE_IP}"
NODE_NAME="${NODE_NAME:-stormcos-boot.g8.lo}"
BIN_DIR="${BIN_DIR:-$HOME/projects}"
MT=x86_64-unknown-linux-musl
SSH="ssh -o StrictHostKeyChecking=no root@$NODE_IP"
SCP="scp -o StrictHostKeyChecking=no"
S="$(mktemp -d)"; mkdir -p "$S/pki"

RK="$BIN_DIR/rustkube/target/$MT/release"
RKN="$BIN_DIR/rustkube-node/target/$MT/release"
FE="$BIN_DIR/fastetcd/target/$MT/release"

echo "== PKI (cluster CA + SA + component/admin client certs + apiserver serving cert) =="
cd "$S/pki"
openssl genrsa -out ca.key 2048 2>/dev/null
openssl req -x509 -new -nodes -key ca.key -subj /CN=kubernetes-ca -days 3650 -out ca.crt 2>/dev/null
openssl genrsa -out sa.key 2048 2>/dev/null; openssl rsa -in sa.key -pubout -out sa.pub 2>/dev/null
gc() { local b=$1 cn=$2 o=${3:-}; local s=/CN=$cn; [ -n "$o" ] && s=/CN=$cn/O=$o
  openssl genrsa -out "$b.key" 2048 2>/dev/null
  openssl req -new -key "$b.key" -subj "$s" -out "$b.csr" 2>/dev/null
  openssl x509 -req -in "$b.csr" -CA ca.crt -CAkey ca.key -CAcreateserial -days 3650 \
    -extfile <(printf extendedKeyUsage=clientAuth) -out "$b.crt" 2>/dev/null; rm -f "$b.csr"; }
gc admin admin system:masters
gc controller-manager system:kube-controller-manager
gc scheduler system:kube-scheduler
gc bootstrap kubelet-bootstrap system:bootstrappers
openssl genrsa -out apiserver.key 2048 2>/dev/null
openssl req -new -key apiserver.key -subj /CN=kube-apiserver -out apiserver.csr 2>/dev/null
openssl x509 -req -in apiserver.csr -CA ca.crt -CAkey ca.key -CAcreateserial -days 3650 -extfile <(cat <<EOF
subjectAltName=DNS:kubernetes,DNS:kubernetes.default,DNS:kubernetes.default.svc,DNS:$NODE_NAME,DNS:localhost,IP:127.0.0.1,IP:$NODE_IP,IP:10.96.0.1
extendedKeyUsage=serverAuth
EOF
) -out apiserver.crt 2>/dev/null; rm -f apiserver.csr

echo "== push binaries, units, PKI =="
$SSH 'mkdir -p /etc/kubernetes/pki /etc/fastetcd /var/lib/fastetcd/data /var/lib/kubernetes'
$SCP "$RK/kube-apiserver" "$RK/kube-controller-manager" "$RK/kube-scheduler" \
     "$RKN/kubelet" "$RKN/kube-proxy" "$FE/fastetcd" root@$NODE_IP:/usr/bin/
$SCP "$S"/pki/{ca.crt,ca.key,sa.key,sa.pub,apiserver.crt,apiserver.key,controller-manager.crt,controller-manager.key,scheduler.crt,scheduler.key,admin.crt,admin.key} \
     root@$NODE_IP:/etc/kubernetes/pki/
# units — strip the service-user the RPM would have created (we run as root here)
sed '/^User=/d;/^Group=/d;/^DynamicUser=/d' "$FE/../../../deploy/systemd/fastetcd.service" 2>/dev/null \
  | $SSH 'cat > /etc/systemd/system/fastetcd.service'
$SCP "$RK/../../../deploy/systemd/kube-apiserver.service" \
     "$RK/../../../deploy/systemd/kube-controller-manager.service" \
     "$RK/../../../deploy/systemd/kube-scheduler.service" \
     "$RKN/../../../deploy/systemd/kubelet.service" root@$NODE_IP:/etc/systemd/system/

echo "== configs + start (DEV: HTTP + anonymous; see rustkube-node#19) =="
$SSH "cat > /etc/fastetcd/fastetcd.conf <<EOF
ETCD_NAME=stormcos
ETCD_DATA_DIR=/var/lib/fastetcd/data
ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379
ETCD_LISTEN_PEER_URLS=http://0.0.0.0:2380
ETCD_INITIAL_ADVERTISE_PEER_URLS=http://$NODE_IP:2380
ETCD_ADVERTISE_CLIENT_URLS=http://$NODE_IP:2379
ETCD_INITIAL_CLUSTER=stormcos=http://$NODE_IP:2380
ETCD_INITIAL_CLUSTER_TOKEN=stormcos
ETCD_INITIAL_CLUSTER_STATE=new
EOF
cat > /etc/kubernetes/kube-apiserver <<EOF
KUBE_APISERVER_ARGS=--etcd-servers http://127.0.0.1:2379 --bind-addr 0.0.0.0 --secure-port 6443 --client-ca-file /etc/kubernetes/pki/ca.crt --service-account-key-file /etc/kubernetes/pki/sa.pub --anonymous-auth true
EOF
cat > /etc/kubernetes/kube-controller-manager <<EOF
KUBE_CONTROLLER_MANAGER_ARGS=--apiserver http://127.0.0.1:6443 --certificate-authority /etc/kubernetes/pki/ca.crt --client-certificate /etc/kubernetes/pki/controller-manager.crt --client-key /etc/kubernetes/pki/controller-manager.key --cluster-signing-cert-file /etc/kubernetes/pki/ca.crt --cluster-signing-key-file /etc/kubernetes/pki/ca.key
EOF
cat > /etc/kubernetes/kube-scheduler <<EOF
KUBE_SCHEDULER_ARGS=--apiserver http://127.0.0.1:6443 --certificate-authority /etc/kubernetes/pki/ca.crt --client-certificate /etc/kubernetes/pki/scheduler.crt --client-key /etc/kubernetes/pki/scheduler.key
EOF
cat > /etc/kubernetes/kubelet <<EOF
KUBELET_ARGS=--apiserver http://127.0.0.1:6443 --node-name $NODE_NAME --runtime native --no-cni
EOF
chmod +x /usr/bin/{fastetcd,kube-apiserver,kube-controller-manager,kube-scheduler,kubelet,kube-proxy}
chmod 600 /etc/kubernetes/pki/*.key
systemctl daemon-reload
systemctl start fastetcd; sleep 3
systemctl start kube-apiserver; sleep 5
systemctl start kube-controller-manager kube-scheduler; sleep 2
systemctl start kubelet; sleep 6
echo 'services:' \$(systemctl is-active fastetcd kube-apiserver kube-controller-manager kube-scheduler kubelet | paste -sd' ')"

rm -rf "$S"
echo "Done. Check: curl -s http://$NODE_IP:6443/api/v1/nodes"
