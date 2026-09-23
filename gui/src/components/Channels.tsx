import { CheckIcon, EllipsisHorizontalIcon, TvIcon } from "@heroicons/react/24/outline";
import { Spinner, Tabs } from "@heroui/react";
import { type JSX, type ReactNode, useEffect, useState } from "react";

import { groupByDeliverySystem, useChannels } from "../api/channels";
import { isSameService, type ServiceKey, serviceKeyId, useServices } from "../api/services";
import { useStream } from "../api/stream";
import { type Channel, DeliverySystem, type Service } from "../gen/chibitv/v1/chibitv_pb";
import { useSelectService, useServiceKey } from "../router";

interface ChannelsProps {
  onServiceChange?: () => void;
}

export function Channels({ onServiceChange }: ChannelsProps): JSX.Element {
  const { state } = useStream();
  const service = useServiceKey();
  const selectServiceKey = useSelectService();
  const [expandedGroupId, setExpandedGroupId] = useState<string>();
  const [selectedDeliverySystem, setSelectedDeliverySystem] = useState<DeliverySystem>();
  const { data: services = [], isLoading: areServicesLoading, isError: areServicesError } = useServices();
  const { data: channels = [], isLoading: areChannelsLoading, isError: areChannelsError } = useChannels();
  const selectService = (selected: ServiceKey) => {
    selectServiceKey(selected);
    onServiceChange?.();
  };
  // The stream is still tuning while the reported service lags the selection.
  const isTuning = service !== undefined && !isSameService(state?.service?.key, service);
  const currentChannelId = services.find(({ key }) => isSameService(key, service))?.channelId;
  const currentDeliverySystem = channels.find((channel) => channel.id === currentChannelId)?.deliverySystem;
  const servicesByChannel = new Map<number, Service[]>();
  for (const listed of services) {
    const channelServices = servicesByChannel.get(listed.channelId) ?? [];
    channelServices.push(listed);
    servicesByChannel.set(listed.channelId, channelServices);
  }

  useEffect(() => {
    if (currentDeliverySystem !== undefined) {
      setSelectedDeliverySystem(currentDeliverySystem);
    }
  }, [currentDeliverySystem]);

  if (areServicesLoading || areChannelsLoading) {
    return (
      <div className="flex flex-1 items-center justify-center gap-3 text-sm text-muted">
        <Spinner size="sm" />
        Loading channels
      </div>
    );
  }

  if (areServicesError || areChannelsError) {
    return <p className="p-3 text-sm text-danger">Could not load channels.</p>;
  }

  const groups = groupByDeliverySystem(channels);

  if (groups.length === 0) {
    return <p className="p-3 text-sm text-muted">No channels are available.</p>;
  }

  // The row is a container rather than the button itself, so that a control of
  // its own can sit inside it; the button stretches over the row to stay the target.
  const renderService = (listed: Service, trailing?: ReactNode) => {
    const selected = isSameService(listed.key, service);
    return (
      <div
        key={listed.key && serviceKeyId(listed.key)}
        className={`group relative flex min-h-16 min-w-0 items-center gap-1 rounded-xl px-3 py-2 transition-colors hover:bg-default ${selected ? "bg-accent-soft text-accent-soft-foreground" : ""}`}
      >
        <button
          type="button"
          aria-pressed={selected}
          disabled={!listed.key}
          onClick={() => listed.key && selectService(listed.key)}
          className="flex min-w-0 flex-1 items-center gap-3 text-start outline-none before:absolute before:inset-0 before:rounded-xl focus-visible:before:outline-2 focus-visible:before:outline-accent"
        >
          <span className="relative flex h-9 w-12 shrink-0 items-center justify-center overflow-hidden rounded bg-white text-gray-400">
            <TvIcon className="size-5" />
            {listed.logoUrl && (
              <img
                key={listed.logoUrl}
                src={listed.logoUrl}
                alt=""
                className="absolute inset-0 h-full w-full bg-white object-contain"
                onError={(event) => {
                  event.currentTarget.hidden = true;
                }}
              />
            )}
          </span>
          <span className="flex min-w-0 flex-1 flex-col gap-0.5">
            <span className="truncate text-sm font-semibold">{listed.name}</span>
            {listed.currentEvent?.title && (
              <span className="line-clamp-2 text-xs text-muted">{listed.currentEvent.title}</span>
            )}
          </span>
          {selected &&
            (isTuning ? (
              <Spinner className="shrink-0" size="sm" />
            ) : (
              <CheckIcon className="size-4 shrink-0 text-accent" />
            ))}
        </button>
        {trailing}
      </div>
    );
  };

  const renderChannels = (groupChannels: Channel[]) => (
    <div className="flex flex-col gap-1">
      {groupChannels.flatMap((channel) => {
        const grouped = new Map<string, Service[]>();
        for (const listed of servicesByChannel.get(channel.id) ?? []) {
          // ponytail: SI has no universal subchannel flag. Group terrestrial
          // services by multiplex and satellite variants by their station name;
          // keep unrelated stations on the same CS multiplex individually visible.
          const name =
            channel.deliverySystem === DeliverySystem.ISDB_T
              ? ""
              : listed.name
                  .normalize("NFKC")
                  .trim()
                  .replace(/[・\s]*\d+$/, "");
          const group = grouped.get(name) ?? [];
          group.push(listed);
          grouped.set(name, group);
        }
        return [...grouped.values()].map(([primary, ...branches]) => {
          if (!primary) return null;
          const groupId = primary.key ? serviceKeyId(primary.key) : String(channel.id);
          const expanded = expandedGroupId === groupId;
          // Keep a selected branch visible even when the rest are collapsed.
          const visibleBranches = expanded ? branches : branches.filter((listed) => isSameService(listed.key, service));
          return (
            <div key={groupId}>
              {renderService(
                primary,
                branches.length > 0 && (
                  <button
                    type="button"
                    aria-label={`${expanded ? "Hide" : "Show"} subchannels for ${primary.name}`}
                    aria-expanded={expanded}
                    aria-controls={`subchannels-${groupId}`}
                    onClick={() => setExpandedGroupId(expanded ? undefined : groupId)}
                    // A mouse reveals it on the row it hovers; touch has no hover, so it stays.
                    className="relative -me-1 flex size-8 shrink-0 items-center justify-center rounded-lg text-muted transition-opacity hover:text-foreground focus-visible:opacity-100 focus-visible:outline-2 focus-visible:outline-accent group-hover:opacity-100 aria-expanded:opacity-100 [@media(hover:hover)_and_(pointer:fine)]:opacity-0"
                  >
                    <EllipsisHorizontalIcon className="size-5" />
                  </button>
                ),
              )}
              <div id={`subchannels-${groupId}`} className="ms-6 flex flex-col gap-1 border-s border-default ps-1">
                {visibleBranches.map((listed) => renderService(listed))}
              </div>
            </div>
          );
        });
      })}
    </div>
  );

  const selectedKey = groups.find((group) => group.id === selectedDeliverySystem)?.id ?? groups[0].id;

  return (
    <Tabs
      className="min-h-0 flex-1"
      selectedKey={selectedKey}
      onSelectionChange={(key) => setSelectedDeliverySystem(Number(key) as DeliverySystem)}
    >
      <Tabs.ListContainer className="shrink-0">
        <Tabs.List aria-label="Broadcast waves">
          {groups.map((group) => (
            <Tabs.Tab key={group.id} id={group.id}>
              <Tabs.Indicator />
              {group.label}
            </Tabs.Tab>
          ))}
        </Tabs.List>
      </Tabs.ListContainer>
      {groups.map((group) => (
        <Tabs.Panel key={group.id} id={group.id} className="min-h-0 flex-1 overflow-y-auto p-0">
          {renderChannels(group.channels)}
        </Tabs.Panel>
      ))}
    </Tabs>
  );
}
