if (response.status !== 200) {
  throw new Error("agent returned HTTP " + response.status);
}

const data = JSON.parse(response.body);
if (data.schema_version !== 1) {
  throw new Error("unsupported agent schema version " + data.schema_version);
}

const ageSeconds = (Date.now() - Date.parse(data.generated_at)) / 1000;
if (!Number.isFinite(ageSeconds) || ageSeconds > 90) {
  throw new Error("agent snapshot is stale: " + ageSeconds + " seconds");
}

function collected(id) {
  const item = data.collectors[id];
  if (!item) {
    throw new Error("required collector is missing: " + id);
  }
  if (item.status !== "collected") {
    throw new Error(id + " failed: " + item.code + " — " + item.summary);
  }
  return item;
}

const disk = collected("disk_root");
if (disk.values.used_ratio >= 0.85) {
  throw new Error("root disk usage is " + Math.round(disk.values.used_ratio * 100) + "%");
}
if (disk.values.inodes_used_ratio >= 0.85) {
  throw new Error("root inode usage is " + Math.round(disk.values.inodes_used_ratio * 100) + "%");
}

const service = collected("fomkee_service");
if (service.facts.active !== true) {
  throw new Error("fomkee.service is " + service.facts.active_state);
}

const backup = collected("backups");
if (backup.values.latest_age > 26 * 60 * 60) {
  throw new Error("latest backup is " + Math.round(backup.values.latest_age / 3600) + " hours old");
}

const postgres = collected("postgres");
if ((postgres.values.temp_bytes_total_delta || 0) > 1024 * 1024 * 1024) {
  throw new Error("PostgreSQL wrote more than 1 GiB of temporary files in one interval");
}
if ((postgres.values.deadlocks_total_delta || 0) > 0) {
  throw new Error("PostgreSQL recorded a new deadlock");
}

const queue = collected("fomkee_queue");
if (queue.values.dead_letters > 0) {
  throw new Error("Fomkee has " + queue.values.dead_letters + " dead letters");
}
if (queue.values.oldest_due_age > 120) {
  throw new Error("oldest due queue item is " + queue.values.oldest_due_age + " seconds old");
}
if ((queue.values.flush_failures_total_delta || 0) > 0) {
  throw new Error("Fomkee recorded a new outbox flush failure");
}
