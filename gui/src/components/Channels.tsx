import { CheckIcon } from "@heroicons/react/24/outline";
import { Disclosure, DisclosureGroup, ListBox, Spinner, Tabs } from "@heroui/react";
import { type JSX, useEffect, useState } from "react";

import { groupByDeliverySystem, useChannels } from "../api/channels";
import { isSameService, type ServiceKey, serviceKeyId, useServices } from "../api/services";
import { useStream } from "../api/stream";
import type { Channel, DeliverySystem, Service } from "../gen/chibitv/v1/chibitv_pb";
import { useSelectService, useServiceKey } from "../router";

interface ChannelsProps {
  onServiceChange?: () => void;
}

export function Channels({ onServiceChange }: ChannelsProps): JSX.Element {
  const { state } = useStream();
  const service = useServiceKey();
  const selectServiceKey = useSelectService();
  const [expandedChannelId, setExpandedChannelId] = useState<number>();
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
    if (currentChannelId !== undefined) {
      setExpandedChannelId(currentChannelId);
    }
  }, [currentChannelId]);

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

  const renderChannels = (groupChannels: Channel[]) => (
    <DisclosureGroup
      className="gap-1"
      // Keys must be strings: react-aria's Disclosure drops a falsy id (`id ||= defaultId`),
      // so the first channel (id 0) would never match its numeric key.
      expandedKeys={expandedChannelId === undefined ? [] : [String(expandedChannelId)]}
      onExpandedChange={(keys) => {
        const [key] = keys;
        if (key === undefined) {
          setExpandedChannelId(undefined);
          return;
        }

        const channelId = Number(key);
        setExpandedChannelId(channelId);

        const firstService = servicesByChannel.get(channelId)?.[0];
        if (firstService?.key && !isSameService(firstService.key, service)) {
          selectService(firstService.key);
        }
      }}
    >
      {groupChannels.map((channel) => {
        const channelServices = servicesByChannel.get(channel.id) ?? [];

        return (
          <Disclosure key={channel.id} id={String(channel.id)} isDisabled={channelServices.length === 0}>
            <Disclosure.Heading>
              <Disclosure.Trigger className="flex min-h-12 w-full flex-row items-center gap-2 rounded-xl px-3 text-sm font-semibold data-[expanded=true]:bg-accent-soft data-[expanded=true]:text-accent-soft-foreground">
                <span className="min-w-0 flex-1 truncate text-start">{channel.name}</span>
                <Disclosure.Indicator className="size-4 shrink-0" />
              </Disclosure.Trigger>
            </Disclosure.Heading>
            <Disclosure.Content>
              <Disclosure.Body className="pb-1 ps-3 pt-1">
                <ListBox
                  aria-label={`${channel.name} services`}
                  className="gap-1 p-0"
                  selectedKeys={service === undefined ? [] : [serviceKeyId(service)]}
                  selectionMode="single"
                  onSelectionChange={(keys) => {
                    if (keys === "all") {
                      return;
                    }

                    const [key] = keys;
                    const selected = channelServices.find(({ key: id }) => id && serviceKeyId(id) === key)?.key;
                    if (selected && !isSameService(selected, service)) {
                      selectService(selected);
                    }
                  }}
                >
                  {channelServices.map((channelService) => (
                    <ListBox.Item
                      key={channelService.key && serviceKeyId(channelService.key)}
                      id={channelService.key && serviceKeyId(channelService.key)}
                      className="min-h-12 rounded-xl px-3 data-[selected=true]:bg-accent-soft data-[selected=true]:text-accent-soft-foreground"
                      textValue={channelService.name}
                    >
                      <div className="flex min-w-0 flex-1 flex-col">
                        <span className="truncate text-sm font-medium">{channelService.name}</span>
                        {channelService.providerName && (
                          <span className="truncate text-xs text-muted">{channelService.providerName}</span>
                        )}
                      </div>
                      {isTuning && isSameService(channelService.key, service) ? (
                        <Spinner className="ms-auto shrink-0" size="sm" />
                      ) : (
                        <ListBox.ItemIndicator className="text-accent">
                          <CheckIcon className="size-4" />
                        </ListBox.ItemIndicator>
                      )}
                    </ListBox.Item>
                  ))}
                </ListBox>
              </Disclosure.Body>
            </Disclosure.Content>
          </Disclosure>
        );
      })}
    </DisclosureGroup>
  );

  const selectedKey = groups.find((group) => group.id === selectedDeliverySystem)?.id ?? groups[0].id;

  return (
    <Tabs
      className="min-h-0 flex-1"
      selectedKey={selectedKey}
      onSelectionChange={(key) => {
        const deliverySystem = Number(key) as DeliverySystem;
        setSelectedDeliverySystem(deliverySystem);

        // Tune to the first service on the wave when the current service is on another wave.
        if (deliverySystem === currentDeliverySystem) {
          return;
        }

        const group = groups.find((group) => group.id === deliverySystem);
        const firstService = group?.channels
          .map((channel) => servicesByChannel.get(channel.id)?.[0])
          .find((first) => first !== undefined);
        if (firstService?.key && !isSameService(firstService.key, service)) {
          setExpandedChannelId(firstService.channelId);
          selectService(firstService.key);
        }
      }}
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
