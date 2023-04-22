# Copyright 2020 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0
"""Basic tests scenarios for snapshot save/restore."""

import json
import os
import platform

import pytest

import host_tools.logging as log_tools
from framework.microvm import SnapshotType
from framework.properties import global_props
from framework.stats import consumer, criteria, function, producer, types
from framework.utils import CpuMap, eager_map, get_kernel_version

# How many latencies do we sample per test.
SAMPLE_COUNT = 3
USEC_IN_MSEC = 1000
PLATFORM = platform.machine()


# Latencies in milliseconds.
# The latency for snapshot creation has high variance due to scheduler noise.
# The issue is tracked here:
# https://github.com/firecracker-microvm/firecracker/issues/2346
# TODO: Update baseline values after fix.
CREATE_LATENCY_BASELINES = {
    ("x86_64", "2vcpu_256mb.json", "FULL"): 180,
    ("x86_64", "2vcpu_256mb.json", "DIFF"): 70,
    ("x86_64", "2vcpu_512mb.json", "FULL"): 280,
    ("x86_64", "2vcpu_512mb.json", "DIFF"): 90,
    ("aarch64", "2vcpu_256mb.json", "FULL"): 160,
    ("aarch64", "2vcpu_256mb.json", "DIFF"): 70,
    ("aarch64", "2vcpu_512mb.json", "FULL"): 300,
    ("aarch64", "2vcpu_512mb.json", "DIFF"): 75,
}

# The latencies for x86 are pretty high due to a design
# in the cgroups V1 implementation in the kernel. We recommend
# switching to cgroups v2 for much lower snap resume latencies.
# More details on this:
# https://github.com/firecracker-microvm/firecracker/issues/2027
# Latencies for snap resume on cgroups V2 can be found in our
# long-running performance configs (i.e. integration_tests/performance/configs).
LOAD_LATENCY_BASELINES = {
    ("m5d.metal", "4.14", "sync", "2vcpu_256mb.json"): 9,
    ("m5d.metal", "4.14", "sync", "2vcpu_512mb.json"): 9,
    ("m5d.metal", "5.10", "sync", "2vcpu_256mb.json"): 70,
    ("m5d.metal", "5.10", "sync", "2vcpu_512mb.json"): 90,
    ("m5d.metal", "5.10", "async", "2vcpu_256mb.json"): 210,
    ("m5d.metal", "5.10", "async", "2vcpu_512mb.json"): 210,
    ("m5d.metal", "6.1", "sync", "2vcpu_256mb.json"): 255,
    ("m5d.metal", "6.1", "sync", "2vcpu_512mb.json"): 245,
    ("m5d.metal", "6.1", "async", "2vcpu_256mb.json"): 245,
    ("m5d.metal", "6.1", "async", "2vcpu_512mb.json"): 225,
    ("m6a.metal", "4.14", "sync", "2vcpu_256mb.json"): 15,
    ("m6a.metal", "4.14", "sync", "2vcpu_512mb.json"): 19,
    ("m6a.metal", "5.10", "sync", "2vcpu_256mb.json"): 75,
    ("m6a.metal", "5.10", "sync", "2vcpu_512mb.json"): 75,
    ("m6a.metal", "5.10", "async", "2vcpu_256mb.json"): 220,
    ("m6a.metal", "5.10", "async", "2vcpu_512mb.json"): 220,
    ("m6a.metal", "6.1", "sync", "2vcpu_256mb.json"): 250,
    ("m6a.metal", "6.1", "sync", "2vcpu_512mb.json"): 250,
    ("m6a.metal", "6.1", "async", "2vcpu_256mb.json"): 250,
    ("m6a.metal", "6.1", "async", "2vcpu_512mb.json"): 300,
    ("m6i.metal", "4.14", "sync", "2vcpu_256mb.json"): 9,
    ("m6i.metal", "4.14", "sync", "2vcpu_512mb.json"): 9,
    ("m6i.metal", "5.10", "sync", "2vcpu_256mb.json"): 70,
    ("m6i.metal", "5.10", "sync", "2vcpu_512mb.json"): 70,
    ("m6i.metal", "5.10", "async", "2vcpu_256mb.json"): 245,
    ("m6i.metal", "5.10", "async", "2vcpu_512mb.json"): 245,
    ("m6i.metal", "6.1", "sync", "2vcpu_256mb.json"): 220,
    ("m6i.metal", "6.1", "sync", "2vcpu_512mb.json"): 250,
    ("m6i.metal", "6.1", "async", "2vcpu_256mb.json"): 220,
    ("m6i.metal", "6.1", "async", "2vcpu_512mb.json"): 220,
    ("m6g.metal", "4.14", "sync", "2vcpu_256mb.json"): 3,
    ("m6g.metal", "4.14", "sync", "2vcpu_512mb.json"): 3,
    ("m6g.metal", "5.10", "sync", "2vcpu_256mb.json"): 3,
    ("m6g.metal", "5.10", "sync", "2vcpu_512mb.json"): 3,
    ("m6g.metal", "5.10", "async", "2vcpu_256mb.json"): 320,
    ("m6g.metal", "5.10", "async", "2vcpu_512mb.json"): 380,
    ("m6g.metal", "6.1", "sync", "2vcpu_256mb.json"): 2,
    ("m6g.metal", "6.1", "sync", "2vcpu_512mb.json"): 3,
    ("m6g.metal", "6.1", "async", "2vcpu_256mb.json"): 2,
    ("m6g.metal", "6.1", "async", "2vcpu_512mb.json"): 3,
    ("c7g.metal", "4.14", "sync", "2vcpu_256mb.json"): 2,
    ("c7g.metal", "4.14", "sync", "2vcpu_512mb.json"): 2,
    ("c7g.metal", "5.10", "sync", "2vcpu_256mb.json"): 2,
    ("c7g.metal", "5.10", "sync", "2vcpu_512mb.json"): 3,
    ("c7g.metal", "5.10", "async", "2vcpu_256mb.json"): 320,
    ("c7g.metal", "5.10", "async", "2vcpu_512mb.json"): 360,
    ("c7g.metal", "6.1", "sync", "2vcpu_256mb.json"): 2,
    ("c7g.metal", "6.1", "sync", "2vcpu_512mb.json"): 3,
    ("c7g.metal", "6.1", "async", "2vcpu_256mb.json"): 2,
    ("c7g.metal", "6.1", "async", "2vcpu_512mb.json"): 3,
}


def snapshot_create_measurements(vm_type, snapshot_type):
    """Define measurements for snapshot create tests."""
    lower_than = {
        "target": CREATE_LATENCY_BASELINES[
            platform.machine(),
            vm_type,
            "FULL" if snapshot_type == SnapshotType.FULL else "DIFF",
        ]
    }

    latency = types.MeasurementDef.create_measurement(
        "latency",
        "ms",
        [function.Max("max")],
        {"max": criteria.LowerThan(lower_than)},
    )

    return [latency]


def snapshot_resume_measurements(vm_type, io_engine):
    """Define measurements for snapshot resume tests."""
    load_latency = {
        "target": LOAD_LATENCY_BASELINES[
            global_props.instance, get_kernel_version(level=1), io_engine, vm_type
        ]
    }

    latency = types.MeasurementDef.create_measurement(
        "latency",
        "ms",
        [function.Max("max")],
        {"max": criteria.LowerThan(load_latency)},
    )

    return [latency]


def snapshot_create_producer(vm, target_version, metrics_fifo, snapshot_type):
    """Produce results for snapshot create tests."""
    vm.make_snapshot(snapshot_type, target_version=target_version)
    metrics = vm.flush_metrics(metrics_fifo)

    if snapshot_type == SnapshotType.FULL:
        value = metrics["latencies_us"]["full_create_snapshot"] / USEC_IN_MSEC
    else:
        value = metrics["latencies_us"]["diff_create_snapshot"] / USEC_IN_MSEC

    print(f"Latency {value} ms")

    return value


def snapshot_resume_producer(microvm_factory, snapshot, metrics_fifo):
    """Produce results for snapshot resume tests."""

    microvm = microvm_factory.build()
    microvm.spawn()
    microvm.restore_from_snapshot(snapshot, resume=True)

    # Attempt to connect to resumed microvm.
    # Verify if guest can run commands.
    exit_code, _, _ = microvm.ssh.execute_command("ls")
    assert exit_code == 0

    value = 0
    # Parse all metric data points in search of load_snapshot time.
    metrics = microvm.get_all_metrics(metrics_fifo)
    for data_point in metrics:
        metrics = json.loads(data_point)
        cur_value = metrics["latencies_us"]["load_snapshot"] / USEC_IN_MSEC
        if cur_value > 0:
            value = cur_value
            break

    print("Latency {value} ms")
    return value


def test_older_snapshot_resume_latency(
    microvm_factory,
    guest_kernel,
    rootfs,
    firecracker_release,
    io_engine,
    st_core,
):
    """
    Test scenario: Older snapshot load performance measurement.

    With each previous firecracker version, create a snapshot and try to
    restore in current version.
    """
    snapshot_type = SnapshotType.FULL
    vcpus, guest_mem_mib = 2, 512
    microvm_cfg = f"{vcpus}vcpu_{guest_mem_mib}mb.json"
    vm = microvm_factory.build(
        guest_kernel,
        rootfs,
        monitor_memory=False,
        fc_binary_path=firecracker_release.path,
        jailer_binary_path=firecracker_release.jailer,
    )
    metrics_fifo_path = os.path.join(vm.path, "metrics_fifo")
    metrics_fifo = log_tools.Fifo(metrics_fifo_path)
    vm.spawn(metrics_path=metrics_fifo_path)
    vm.basic_config(vcpu_count=vcpus, mem_size_mib=guest_mem_mib)
    vm.add_net_iface()
    vm.start()
    # Check if guest works.
    exit_code, _, _ = vm.ssh.execute_command("ls")
    assert exit_code == 0
    snapshot = vm.make_snapshot(snapshot_type)

    st_core.name = "older_snapshot_resume_latency"
    st_core.iterations = SAMPLE_COUNT
    st_core.custom["guest_config"] = microvm_cfg.strip(".json")
    st_core.custom["io_engine"] = io_engine
    st_core.custom["snapshot_type"] = (
        "FULL" if snapshot_type == SnapshotType.FULL else "DIFF"
    )

    prod = producer.LambdaProducer(
        func=snapshot_resume_producer,
        func_kwargs={
            "microvm_factory": microvm_factory,
            "snapshot": snapshot,
            "metrics_fifo": metrics_fifo,
        },
    )

    cons = consumer.LambdaConsumer(
        func=lambda cons, result: cons.consume_stat(
            st_name="max", ms_name="latency", value=result
        ),
        func_kwargs={},
    )
    eager_map(
        cons.set_measurement_def,
        snapshot_resume_measurements(microvm_cfg, io_engine.lower()),
    )

    st_core.add_pipe(producer=prod, consumer=cons, tag=microvm_cfg)
    # Gather results and verify pass criteria.
    st_core.run_exercise()


@pytest.mark.parametrize("guest_mem_mib", [256, 512])
@pytest.mark.parametrize("snapshot_type", [SnapshotType.FULL, SnapshotType.DIFF])
def test_snapshot_create_latency(
    microvm_factory,
    guest_kernel,
    rootfs,
    guest_mem_mib,
    snapshot_type,
    firecracker_release,
    st_core,
):
    """
    Test scenario: Full/Diff snapshot create performance measurement.

    Testing matrix:
    - Guest kernel: all supported ones
    - Rootfs: Ubuntu 18.04
    - Microvm: 2vCPU with 256/512 MB RAM
    TODO: Multiple microvm sizes must be tested in the async pipeline.
    """
    diff_snapshots = snapshot_type == SnapshotType.DIFF
    vcpus = 2
    microvm_cfg = f"{vcpus}vcpu_{guest_mem_mib}mb.json"
    vm = microvm_factory.build(guest_kernel, rootfs, monitor_memory=False)
    metrics_fifo_path = os.path.join(vm.path, "metrics_fifo")
    metrics_fifo = log_tools.Fifo(metrics_fifo_path)
    vm.spawn(metrics_path=metrics_fifo_path)
    vm.basic_config(
        vcpu_count=vcpus,
        mem_size_mib=guest_mem_mib,
        track_dirty_pages=diff_snapshots,
    )
    vm.start()

    # Check if the needed CPU cores are available. We have the API
    # thread, VMM thread and then one thread for each configured vCPU.
    assert CpuMap.len() >= 2 + vm.vcpus_count

    # Pin uVM threads to physical cores.
    current_cpu_id = 0
    assert vm.pin_vmm(current_cpu_id), "Failed to pin firecracker thread."
    current_cpu_id += 1
    assert vm.pin_api(current_cpu_id), "Failed to pin fc_api thread."
    for idx_vcpu in range(vm.vcpus_count):
        current_cpu_id += 1
        assert vm.pin_vcpu(
            idx_vcpu, current_cpu_id + idx_vcpu
        ), f"Failed to pin fc_vcpu {idx_vcpu} thread."

    st_core.name = f"snapshot_create_{snapshot_type}_latency"
    st_core.iterations = SAMPLE_COUNT
    st_core.custom["guest_config"] = microvm_cfg.strip(".json")
    st_core.custom["snapshot_type"] = (
        "FULL" if snapshot_type == SnapshotType.FULL else "DIFF"
    )

    prod = producer.LambdaProducer(
        func=snapshot_create_producer,
        func_kwargs={
            "vm": vm,
            "target_version": firecracker_release.snapshot_version,
            "metrics_fifo": metrics_fifo,
            "snapshot_type": snapshot_type,
        },
    )

    cons = consumer.LambdaConsumer(
        func=lambda cons, result: cons.consume_stat(
            st_name="max", ms_name="latency", value=result
        ),
        func_kwargs={},
    )
    eager_map(
        cons.set_measurement_def,
        snapshot_create_measurements(microvm_cfg, snapshot_type),
    )

    st_core.add_pipe(producer=prod, consumer=cons, tag=microvm_cfg)
    # Gather results and verify pass criteria.
    st_core.run_exercise()


@pytest.mark.parametrize("guest_mem_mib", [256, 512])
@pytest.mark.parametrize("snapshot_type", [SnapshotType.FULL, SnapshotType.DIFF])
def test_snapshot_resume_latency(
    microvm_factory,
    guest_kernel,
    rootfs,
    guest_mem_mib,
    snapshot_type,
    io_engine,
    st_core,
):
    """
    Test scenario: Snapshot load performance measurement.

    Testing matrix:
    - Guest kernel: All supported ones
    - Rootfs: Ubuntu 18.04
    - Microvm: 2vCPU with 256/512 MB RAM
    TODO: Multiple microvm sizes must be tested in the async pipeline.
    """
    diff_snapshots = snapshot_type == SnapshotType.DIFF
    vcpus = 2
    microvm_cfg = f"{vcpus}vcpu_{guest_mem_mib}mb.json"
    vm = microvm_factory.build(guest_kernel, rootfs, monitor_memory=False)
    metrics_fifo_path = os.path.join(vm.path, "metrics_fifo")
    metrics_fifo = log_tools.Fifo(metrics_fifo_path)
    vm.spawn(metrics_path=metrics_fifo_path)
    vm.basic_config(
        vcpu_count=vcpus,
        mem_size_mib=guest_mem_mib,
        track_dirty_pages=diff_snapshots,
        rootfs_io_engine=io_engine,
    )
    vm.add_net_iface()
    vm.start()
    # Check if guest works.
    exit_code, _, _ = vm.ssh.execute_command("ls")
    assert exit_code == 0

    # Create a snapshot builder from a microvm.
    snapshot = vm.make_snapshot(snapshot_type)
    vm.kill()

    st_core.name = "snapshot_resume_latency"
    st_core.iterations = SAMPLE_COUNT
    st_core.custom["guest_config"] = microvm_cfg.strip(".json")
    st_core.custom["io_engine"] = io_engine
    st_core.custom["snapshot_type"] = (
        "FULL" if snapshot_type == SnapshotType.FULL else "DIFF"
    )

    prod = producer.LambdaProducer(
        func=snapshot_resume_producer,
        func_kwargs={
            "microvm_factory": microvm_factory,
            "snapshot": snapshot,
            "metrics_fifo": metrics_fifo,
        },
    )

    cons = consumer.LambdaConsumer(
        func=lambda cons, result: cons.consume_stat(
            st_name="max", ms_name="latency", value=result
        ),
        func_kwargs={},
    )
    eager_map(
        cons.set_measurement_def,
        snapshot_resume_measurements(microvm_cfg, io_engine.lower()),
    )

    st_core.add_pipe(producer=prod, consumer=cons, tag=microvm_cfg)
    # Gather results and verify pass criteria.
    st_core.run_exercise()
