import Foundation
import Virtualization
import Darwin

// Experimental kernel-realm manager. No disk, network, shared directory, VSOCK,
// clipboard or other external execution authority is configured. The state file
// is written by this trusted host process, never inferred from guest console text.
final class Realm: NSObject, VZVirtualMachineDelegate {
    let stateURL: URL
    let realmID: String
    let fenceURL: URL
    let controlURL: URL
    var detachedShares = false
    let shareMode: String
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
    var firstStopTick: Int?
    var status = "CONFIGURING"

    init(spec: [String: Any]) throws {
        let executable = URL(fileURLWithPath: CommandLine.arguments[0]).resolvingSymlinksInPath()
        let lab = executable.deletingLastPathComponent()
        func local(_ key: String) throws -> URL {
            guard let value = spec[key] as? String else { throw NSError(domain: "missing-" + key, code: 1) }
            let url = URL(fileURLWithPath: value).standardizedFileURL.resolvingSymlinksInPath()
            guard url.path.hasPrefix(lab.path + "/") else { throw NSError(domain: "outside-disposable-lab", code: 1) }
            return url
        }
        let kernel = try local("kernel")
        let ramdisk = try local("initramfs")
        let fixture = try local("fixture")
        guard fixture.path.hasPrefix(lab.appendingPathComponent("projects").path + "/"),
              FileManager.default.fileExists(atPath: fixture.appendingPathComponent(".relay-disposable-project").path),
              let identity = spec["identity"] as? String, UUID(uuidString: identity) != nil,
              let mode = spec["mode"] as? String, ["direct-rw", "readonly-draft"].contains(mode)
        else { throw NSError(domain: "disposable-project-required", code: 1) }
        self.realmID = identity
        self.shareMode = mode
        self.controlURL = try local("control")
        self.fenceURL = controlURL.appendingPathComponent("stop")
        self.stateURL = controlURL.appendingPathComponent("state.json")
        self.label = spec["label"] as? String ?? "research"
        let script = try local("script")
        let payload = try Data(contentsOf: script)
        guard payload.count <= 1024 else { throw NSError(domain: "script-limit", code: 1) }
        let encoded = payload.base64EncodedString()
        super.init()
        let configuration = VZVirtualMachineConfiguration()
        configuration.cpuCount = 1
        configuration.memorySize = UInt64(spec["memoryMiB"] as? Int ?? 192) * 1024 * 1024
        configuration.platform = VZGenericPlatformConfiguration()
        let boot = VZLinuxBootLoader(kernelURL: kernel)
        boot.initialRamdiskURL = ramdisk
        boot.commandLine = "console=hvc0 loglevel=3 panic=1 relay.label=\(label) relay.script=\(encoded)"
        configuration.bootLoader = boot
        let serial = VZVirtioConsoleDeviceSerialPortConfiguration()
        serial.attachment = VZFileHandleSerialPortAttachment(
            fileHandleForReading: FileHandle.standardInput,
            fileHandleForWriting: FileHandle.standardOutput)
        configuration.serialPorts = [serial]
        configuration.entropyDevices = [VZVirtioEntropyDeviceConfiguration()]
        configuration.networkDevices = []
        configuration.storageDevices = []
        var shares: [VZDirectorySharingDeviceConfiguration] = []
        let base = VZVirtioFileSystemDeviceConfiguration(tag: "project")
        base.share = VZSingleDirectoryShare(directory: VZSharedDirectory(url: fixture.appendingPathComponent("seed"), readOnly: mode == "readonly-draft"))
        shares.append(base)
        if mode == "readonly-draft" {
            let draft = VZVirtioFileSystemDeviceConfiguration(tag: "draft")
            draft.share = VZSingleDirectoryShare(directory: VZSharedDirectory(url: fixture.appendingPathComponent("draft"), readOnly: false))
            shares.append(draft)
        }
        configuration.directorySharingDevices = shares
        configuration.socketDevices = []
        try configuration.validate()
        machine = VZVirtualMachine(configuration: configuration)
        machine?.delegate = self
        persist()
    }

    func persist(error: String? = nil) {
        var result: [String: Any] = [
            "scope": "DISPOSABLE_PROJECT_SHARING_EXPERIMENT",
            "realmID": realmID, "label": label, "status": status,
            "managerPID": ProcessInfo.processInfo.processIdentifier,
            "ticks": ticks, "admissionClosed": closed,
            "vmRunning": machine?.state == .running,
            "vmStopped": started && machine?.state == .stopped,
            "networkDevices": 0, "sharedDirectories": shareMode == "direct-rw" ? 1 : 2,
            "shareMode": shareMode, "sharesDetached": detachedShares,
            "workspaceQuiescence": "UNPROVEN_HOST_IO_DRAIN",
            "storageDevices": 0, "socketDevices": 0,
            "nativeExecutionIntegrated": false,
            "nativeExecutionScope": "OPT_IN_USER_SHELL_RAM_ONLY_LINUX_GUEST",
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
            if FileManager.default.fileExists(atPath: self.controlURL.appendingPathComponent("detach").path) && !self.detachedShares {
                for device in self.machine?.directorySharingDevices ?? [] {
                    (device as? VZVirtioFileSystemDevice)?.share = nil
                }
                self.detachedShares = true
            }
            if FileManager.default.fileExists(atPath: self.controlURL.appendingPathComponent("crash").path) {
                self.status = "CRASHING_UNKNOWN"
                self.persist()
                kill(getpid(), SIGKILL)
            }
            if FileManager.default.fileExists(atPath: self.fenceURL.path)
                || FileManager.default.fileExists(atPath: self.stateURL.deletingLastPathComponent().appendingPathComponent("cancel").path) { self.closed = true }
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
                if FileManager.default.fileExists(atPath: self.fenceURL.path)
                || FileManager.default.fileExists(atPath: self.stateURL.deletingLastPathComponent().appendingPathComponent("cancel").path) { self.closed = true }
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
        // Research fault injection, outside the guest: exercise a stop that
        // takes longer than native Core's ordinary 100 ms interruption grace.
        let delayURL = stateURL.deletingLastPathComponent().appendingPathComponent("test-stop-delay-ms")
        if let text = try? String(contentsOf: delayURL, encoding: .utf8),
           let delay = Int(text), delay > 0, delay <= 1000 {
            if firstStopTick == nil { firstStopTick = ticks }
            if ticks - (firstStopTick ?? ticks) < (delay + 99) / 100 { persist(); return }
        }
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
if args.count != 2 { exit(64) }
do {
    let data = try Data(contentsOf: URL(fileURLWithPath: args[1]))
    guard let spec = try JSONSerialization.jsonObject(with: data) as? [String: Any] else { exit(64) }
    let realm = try Realm(spec: spec)
    realm.run()
    withExtendedLifetime(realm) { RunLoop.main.run() }
} catch {
    fputs("workspace VM configuration failed: \(error)\n", stderr)
    exit(2)
}
