#!/usr/bin/env bash
#
# provision-vm.sh — создать VM бота на Proxmox из cloud-образа (issue #70).
#
# Закрывает шаг 1 DEPLOY.md, который до сих пор был единственным ручным:
# «создай VM, поставь ОС как обычно». После вайпа гипервизора 2026-07-29
# именно он и не пережил — VM была наследием, а не артефактом кода.
#
# Запускать НА НОДЕ Proxmox (нужны qm/pvesm). Идемпотентности нет намеренно:
# создание VM с существующим VMID — ошибка, а не тихий no-op. Пересоздать —
# сначала `qm destroy <VMID>` осознанно.
#
# Секретов не принимает и не пишет: cloud-init кладёт всё, кроме .env,
# который доставляется на живую VM отдельно (DEPLOY.md шаг 5).
#
# Параметры через env:
#   VMID          (9001)              — свободный ID вне диапазона полигона 400-499
#   VM_NAME       (njuska-bot)
#   STORAGE       (local-zfs)         — где живёт диск VM
#   SNIPPET_STORE (local)             — datastore с content-type snippets
#   BRIDGE        (vmbr0)
#   VLAN_TAG      (41)                — сегмент полигона; пусто = без тега
#   IP_CIDR       (192.168.41.200/24) — статика; в VLAN 41 DHCP нет
#   GATEWAY       (192.168.41.1)      — шлюз сегмента на RB5009
#   NAMESERVER    (1.1.1.1 8.8.8.8)   — публичные: резолвер роутера из 41
#                                       недоступен by design (input к RB закрыт)
#   SEC_GROUP     (polygon)           — PVE security group на NIC; пусто = не вешать
#   MEMORY_MB     (512)
#   CORES         (1)
#   DISK_GB       (5)
#   IMAGE_URL     (Debian 12 generic cloud amd64)
#   SSH_KEYFILE   (~/.ssh/authorized_keys) — ключи для пользователя admin
#
# Требование к SNIPPET_STORE: включён content-type snippets, иначе qm не
# примет cicustom. Проверяется ниже явно — сообщение понятнее, чем отказ qm.

set -euo pipefail

VMID="${VMID:-9001}"
VM_NAME="${VM_NAME:-njuska-bot}"
STORAGE="${STORAGE:-local-zfs}"
SNIPPET_STORE="${SNIPPET_STORE:-local}"
BRIDGE="${BRIDGE:-vmbr0}"
VLAN_TAG="${VLAN_TAG:-41}"
IP_CIDR="${IP_CIDR:-192.168.41.200/24}"
GATEWAY="${GATEWAY:-192.168.41.1}"
NAMESERVER="${NAMESERVER:-1.1.1.1 8.8.8.8}"
SEC_GROUP="${SEC_GROUP:-polygon}"
MEMORY_MB="${MEMORY_MB:-512}"
CORES="${CORES:-1}"
DISK_GB="${DISK_GB:-5}"
IMAGE_URL="${IMAGE_URL:-https://cloud.debian.org/images/cloud/bookworm/latest/debian-12-generic-amd64.qcow2}"
SSH_KEYFILE="${SSH_KEYFILE:-$HOME/.ssh/authorized_keys}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
USER_DATA="$SCRIPT_DIR/cloud-init/njuska-vm.yaml"

die() { echo "provision-vm: $*" >&2; exit 1; }

command -v qm >/dev/null 2>&1 || die "qm не найден — скрипт запускается на ноде Proxmox, не на маке"
[ -f "$USER_DATA" ] || die "нет $USER_DATA"
[ -f "$SSH_KEYFILE" ] || die "нет $SSH_KEYFILE — задать SSH_KEYFILE (ключи для входа на VM)"
qm status "$VMID" >/dev/null 2>&1 && die "VMID $VMID уже занят — выбрать другой или снести осознанно (qm destroy $VMID)"

pvesm status --storage "$SNIPPET_STORE" >/dev/null 2>&1 \
    || die "хранилище $SNIPPET_STORE не найдено"
pvesm status --content snippets 2>/dev/null | grep -q "^$SNIPPET_STORE " \
    || die "у $SNIPPET_STORE нет content-type snippets — включить: pvesm set $SNIPPET_STORE --content <текущее>,snippets"

snippet_dir="/var/lib/vz/snippets"
[ -d "$snippet_dir" ] || die "нет $snippet_dir — снипеты лежат не там, поправить snippet_dir в скрипте"
install -m 0644 "$USER_DATA" "$snippet_dir/njuska-vm.yaml"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
image="$work/$(basename "$IMAGE_URL")"
echo "==> cloud-образ"
curl -L --fail "$IMAGE_URL" -o "$image"

echo "==> создаю VM $VMID ($VM_NAME)"
net="virtio,bridge=$BRIDGE"
[ -n "$VLAN_TAG" ] && net="$net,tag=$VLAN_TAG"
[ -n "$SEC_GROUP" ] && net="$net,firewall=1"

qm create "$VMID" \
    --name "$VM_NAME" \
    --memory "$MEMORY_MB" \
    --cores "$CORES" \
    --net0 "$net" \
    --scsihw virtio-scsi-single \
    --ostype l26 \
    --agent enabled=1

# Импорт диска: с PVE 8 канонично `--scsi0 <storage>:0,import-from=<файл>`,
# старый `qm importdisk` в 9.x может отсутствовать. Пробуем современную форму,
# при отказе откатываемся на legacy — так скрипт живёт и на старых нодах.
if ! qm set "$VMID" --scsi0 "$STORAGE:0,import-from=$image" 2>/dev/null; then
    echo "provision-vm: import-from не принят, пробую legacy importdisk"
    qm importdisk "$VMID" "$image" "$STORAGE"
    qm set "$VMID" --scsi0 "$STORAGE:vm-$VMID-disk-0"
fi
qm disk resize "$VMID" scsi0 "${DISK_GB}G"
qm set "$VMID" --boot order=scsi0
qm set "$VMID" --ide2 "$STORAGE:cloudinit"
qm set "$VMID" --serial0 socket --vga serial0
qm set "$VMID" --ciuser admin --sshkeys "$SSH_KEYFILE"
# Сеть задаём через ipconfig0, а не в user-data: сетевой конфиг cloud-init
# приходит отдельным источником (network-config), который Proxmox генерирует
# сам — netplan-блок внутри user-data был бы просто проигнорирован.
qm set "$VMID" --ipconfig0 "ip=$IP_CIDR,gw=$GATEWAY"
qm set "$VMID" --nameserver "$NAMESERVER"
qm set "$VMID" --cicustom "user=$SNIPPET_STORE:snippets/njuska-vm.yaml"

# Второй эшелон: та же security group, что tofu вешает на раннеры. Без неё
# VM жила бы в полигонном сегменте без полигонной защиты — контракт RB5009
# (первый эшелон) её бы прикрыл, но на NIC не осталось бы ничего.
if [ -n "$SEC_GROUP" ]; then
    if grep -q "^\[group $SEC_GROUP\]" /etc/pve/firewall/cluster.fw 2>/dev/null; then
        cat > "/etc/pve/firewall/$VMID.fw" <<FW
[OPTIONS]
enable: 1
policy_in: DROP
policy_out: DROP

[RULES]
GROUP $SEC_GROUP
FW
        echo "provision-vm: security group $SEC_GROUP привязана к $VMID"
    else
        echo "provision-vm: ВНИМАНИЕ — группы [$SEC_GROUP] нет в cluster.fw," \
             "VM останется без второго эшелона; проверить polygon-iac (#25)" >&2
    fi
fi

echo "==> старт"
qm start "$VMID"

cat <<EOF

VM $VMID ($VM_NAME) создана и запущена.

cloud-init на первой загрузке поставит пакеты, заведёт пользователя njuska,
скачает бинарь из последнего релиза и разложит systemd-юниты. Занимает
пару минут; следить — qm terminal $VMID (выход: Ctrl+O).

Дальше, по DEPLOY.md шаг 5, остаётся единственное ручное действие —
секреты:

  ssh admin@${IP_CIDR%%/*}
  sudo -u njuska vim /opt/njuska-auto-bot/.env
  sudo systemctl start njuska-auto-bot
  sudo journalctl -u njuska-auto-bot -f

.env сюда не приезжает намеренно: cloud-init-снипет читается из datastore
всеми, у кого есть доступ к хранилищу.
EOF
