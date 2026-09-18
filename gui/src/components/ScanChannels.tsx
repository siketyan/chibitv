import { MagnifyingGlassIcon } from "@heroicons/react/24/outline";
import { Button, Modal, ProgressBar } from "@heroui/react";
import type { UseMutationResult } from "@tanstack/react-query";
import { type JSX, useState } from "react";

import { useRunningScan, useSaveScanResult, useScanChannels, useScanResult } from "../api/scan";
import { useStartTaskError } from "../api/tasks";
import { type Channel, DeliverySystem, type ScannedChannel } from "../gen/chibitv/v1/chibitv_pb";

const DELIVERY_SYSTEMS: { id: DeliverySystem; label: string }[] = [
  { id: DeliverySystem.ISDB_T, label: "Terrestrial" },
  { id: DeliverySystem.ISDB_S, label: "BS/CS" },
  { id: DeliverySystem.ISDB_S3, label: "BS 4K" },
];

/** How long a scan is given on each channel, which the server caps. */
const TIMEOUT_SECONDS = 12;

/** The same, for a fast scan, which waits for a whole network on one channel. */
const FAST_TIMEOUT_SECONDS = 30;

/** Looks for the channels on air and shows what was found. */
export function ScanChannels(): JSX.Element {
  const [isOpen, setIsOpen] = useState(false);
  const [deliverySystem, setDeliverySystem] = useState(DeliverySystem.ISDB_T);
  const [isFast, setIsFast] = useState(false);
  const scanChannels = useScanChannels();
  const saveScanResult = useSaveScanResult();
  const runningScan = useRunningScan();
  const startError = useStartTaskError();
  const result = useScanResult();
  // A fast scan reads a network out of one transponder, which the terrestrial
  // channels do not share.
  const isSatellite = deliverySystem !== DeliverySystem.ISDB_T;
  const fast = isFast && isSatellite;
  const percentage = runningScan?.progress === undefined ? undefined : Math.round(runningScan.progress * 100);

  return (
    <Modal isOpen={isOpen} onOpenChange={setIsOpen}>
      <Button aria-label="Scan for channels" isIconOnly size="sm" variant="ghost">
        <MagnifyingGlassIcon />
      </Button>
      <Modal.Backdrop variant="blur">
        <Modal.Container placement="center" size="lg">
          <Modal.Dialog>
            <Modal.Header>
              <Modal.Heading className="pe-8">Scan for channels</Modal.Heading>
            </Modal.Header>
            <Modal.CloseTrigger />
            <Modal.Body>
              <div className="flex flex-col gap-4">
                <div className="flex flex-wrap items-center gap-2">
                  {DELIVERY_SYSTEMS.map(({ id, label }) => (
                    <Button
                      key={id}
                      aria-pressed={deliverySystem === id}
                      isDisabled={runningScan !== undefined}
                      size="sm"
                      variant={deliverySystem === id ? "primary" : "ghost"}
                      onPress={() => setDeliverySystem(id)}
                    >
                      {label}
                    </Button>
                  ))}
                  <Button
                    aria-pressed={fast}
                    className="ms-auto"
                    isDisabled={!isSatellite || runningScan !== undefined}
                    size="sm"
                    variant={fast ? "primary" : "ghost"}
                    onPress={() => setIsFast(!isFast)}
                  >
                    Fast
                  </Button>
                </div>
                <p className="text-xs text-muted">
                  {isSatellite
                    ? "A satellite network describes itself, so a fast scan reads the whole list off one transponder per network instead of tuning to every stream."
                    : "Every UHF physical channel is tuned in turn, which takes a few minutes."}
                </p>
                <Button
                  isDisabled={runningScan !== undefined || scanChannels.isPending}
                  onPress={() => {
                    // What the last scan was saved as says nothing about what
                    // this one is about to find.
                    saveScanResult.reset();
                    scanChannels.mutate({
                      deliverySystem,
                      fast,
                      timeoutSeconds: fast ? FAST_TIMEOUT_SECONDS : TIMEOUT_SECONDS,
                    });
                  }}
                >
                  {runningScan === undefined ? "Start scanning" : "Scanning"}
                </Button>
                {runningScan && (
                  <div className="flex flex-col gap-2">
                    <ProgressBar
                      aria-label="Scan progress"
                      isIndeterminate={percentage === undefined}
                      size="sm"
                      value={percentage}
                    >
                      <ProgressBar.Track>
                        <ProgressBar.Fill />
                      </ProgressBar.Track>
                    </ProgressBar>
                    <p className="truncate text-xs text-muted">
                      {runningScan.message || "Starting"}
                      {percentage === undefined ? "" : ` · ${percentage}%`}
                    </p>
                  </div>
                )}
                {startError && <p className="text-xs text-danger">Could not start the scan: {startError}</p>}
                {result && result.channels.length > 0 && (
                  <ScanResult channels={result.channels} save={saveScanResult} />
                )}
              </div>
            </Modal.Body>
          </Modal.Dialog>
        </Modal.Container>
      </Modal.Backdrop>
    </Modal>
  );
}

function ScanResult({
  channels,
  save,
}: {
  channels: ScannedChannel[];
  save: UseMutationResult<Channel[], Error, void>;
}): JSX.Element {
  return (
    <div className="flex min-h-0 flex-col gap-2">
      <div className="flex items-center gap-2">
        <h3 className="mr-auto text-sm font-medium">
          {channels.length} channel{channels.length === 1 ? "" : "s"} found
        </h3>
        <Button isDisabled={save.isPending} size="sm" onPress={() => save.mutate()}>
          {save.isPending ? "Saving" : "Save channels"}
        </Button>
      </div>
      {/* Saving replaces the channels of the broadcast that was scanned, and
          leaves the channels of the other broadcasts alone. */}
      <p className="text-xs text-muted">
        Saving these replaces the channels kept for this broadcast. The other broadcasts are left alone.
      </p>
      {save.isSuccess && <p className="text-xs text-success">Saved. These channels are being served now.</p>}
      {save.error && <p className="text-xs text-danger">Could not save the channels: {save.error.message}</p>}
      <ul className="flex max-h-48 flex-col gap-1 overflow-auto">
        {channels.map((channel) => (
          <li
            key={`${channel.deliverySystem}-${channel.frequency}-${channel.streamId ?? 0}`}
            className="rounded-lg border border-white/10 bg-white/5 px-3 py-2"
          >
            <p className="truncate text-sm font-medium">{channel.name}</p>
            <p className="truncate text-xs text-muted">
              {channel.services.map((service) => service.name).join(" · ") || "No services"}
            </p>
          </li>
        ))}
      </ul>
    </div>
  );
}
