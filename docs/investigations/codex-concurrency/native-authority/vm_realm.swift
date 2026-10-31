import Foundation
import Virtualization
import Darwin

// Experimental kernel-realm manager. No disk, network, shared directory, VSOCK,
// clipboard or other external execution authority is configured. The state file
// is written by this trusted host process, never inferred from guest console text.
final class Realm: NSObject, VZVirtualMachineDelegate {
    let stateURL: URL
    let realmID = UUID().uuidString
    let label: String
    var machine: VZVirtualMachine?
    var timer: DispatchSourceTimer?
    var signalSource: DispatchSourceSignal?
    var started = false
    var closed = false
    var stopCalled = false
    var finished = false
    var evidenceFailed = false
    var ticks = 0
    var status = "CONFIGURING"

    init(kernel: URL, ramdisk: URL, label: String, state: URL) throws {
        self.stateURL = state
        self.label = label
        super.init()
        let configuration = VZVirtualMachineConfiguration()
        configuration.cpuCount = 1
        configuration.memorySize = 192 * 1024 * 1024
        configuration.platform = VZGenericPlatformConfiguration()
        let boot = VZLinuxBootLoader(kernelURL: kernel)
        boot.initialRamdiskURL = ramdisk
        boot.commandLine = "console=hvc0 loglevel=3 panic=1 relay.label=\(label)"
        configuration.bootLoader = boot
        let serial = VZVirtioConsoleDeviceSerialPortConfiguration()
        serial.attachment = VZFileHandleSerialPortAttachment(
            fileHandleForReading: FileHandle.standardInput,
            fileHandleForWriting: FileHandle.standardOutput)
        configuration.serialPorts = [serial]
        configuration.entropyDevices = [VZVirtioEntropyDeviceConfiguration()]
        configuration.networkDevices = []
        configuration.storageDevices = []
        configuration.directorySharingDevices = []
        configuration.socketDevices = []
        try configuration.validate()
        machine = VZVirtualMachine(configuration: configuration)
        machine?.delegate = self
        persist()
    }

    func persist(error: String? = nil) {
        var result: [String: Any] = [
            "scope": "KERNEL_REALM_WITHOUT_EXTERNAL_CAPABILITIES",
            "realmID": realmID, "label": label, "status": status,
            "managerPID": ProcessInfo.processInfo.processIdentifier,
            "ticks": ticks, "admissionClosed": closed,
            "vmRunning": machine?.state == .running,
            "vmStopped": started && machine?.state == .stopped,
            "networkDevices": 0, "sharedDirectories": 0,
            "storageDevices": 0, "socketDevices": 0,
            "nativeExecutionIntegrated": false,
            "handoffPermitted": false,
        ]
        if let error = error { result["error"] = error }
        do {
            let data = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
            try data.write(to: stateURL, options: [.atomic])
        } catch {
            // Evidence failure closes admission and stops this exact VM.
            closed = true
            evidenceFailed = true
            DispatchQueue.main.async { [weak self] in self?.requestStop() }
        }
    }

    func run() {
        signal(SIGTERM, SIG_IGN)
        let source = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
        source.setEventHandler { [weak self] in self?.requestStop() }
        source.resume()
        signalSource = source
        let pulse = DispatchSource.makeTimerSource(queue: .main)
        pulse.schedule(deadline: .now(), repeating: .milliseconds(100))
        pulse.setEventHandler { [weak self] in
            guard let self = self, !self.finished else { return }
            self.ticks += 1
            if self.closed { self.requestStop() }
            if self.ticks >= 200 { self.requestStop() }
            if self.ticks >= 260 {
                self.status = "STOP_UNPROVEN"
                self.persist(error: "bounded manager shutdown deadline")
                exit(2)
            }
            self.persist()
        }
        pulse.resume()
        timer = pulse
        status = "STARTING"
        persist()
        machine?.start { [weak self] result in
            guard let self = self else { return }
            switch result {
            case .success:
                self.started = true
                self.status = "RUNNING"
                self.persist()
                if self.closed { self.requestStop() }
            case .failure(let error):
                self.status = "START_FAILED"
                self.persist(error: error.localizedDescription)
                exit(2)
            }
        }
    }

    func requestStop() {
        closed = true
        guard !finished, !stopCalled, let vm = machine else { return }
        status = "STOP_REQUESTED"
        guard vm.canStop else {
            if !evidenceFailed { persist() }
            return
        }
        stopCalled = true
        persist()
        vm.stop { [weak self] error in
            guard let self = self, !self.finished else { return }
            if let error = error {
                self.status = "STOP_FAILED"
                self.persist(error: error.localizedDescription)
                exit(2)
            }
            self.finished = true
            self.status = vm.state == .stopped ? "STOPPED" : "STOP_UNPROVEN"
            self.persist()
            exit(vm.state == .stopped && !self.evidenceFailed ? 0 : 2)
        }
    }

    func guestDidStop(_ virtualMachine: VZVirtualMachine) {
        guard !finished else { return }
        closed = true
        finished = true
        status = "GUEST_STOPPED"
        persist()
        exit(virtualMachine.state == .stopped && !evidenceFailed ? 0 : 2)
    }

    func virtualMachine(_ virtualMachine: VZVirtualMachine, didStopWithError error: Error) {
        closed = true
        finished = true
        status = "VM_FAILED"
        persist(error: error.localizedDescription)
        exit(2)
    }
}

let args = CommandLine.arguments
if args.count != 5 || !["A", "B"].contains(args[3]) { exit(64) }
do {
    let realm = try Realm(kernel: URL(fileURLWithPath: args[1]),
                          ramdisk: URL(fileURLWithPath: args[2]), label: args[3],
                          state: URL(fileURLWithPath: args[4]))
    realm.run()
    withExtendedLifetime(realm) { RunLoop.main.run() }
} catch {
    let result: [String: Any] = ["status": "CONFIGURATION_FAILED", "error": error.localizedDescription,
                                 "handoffPermitted": false, "nativeExecutionIntegrated": false]
    if let data = try? JSONSerialization.data(withJSONObject: result), args.count == 5 {
        try? data.write(to: URL(fileURLWithPath: args[4]), options: [.atomic])
    }
    exit(2)
}
