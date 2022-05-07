// SPDX-License-Identifier: MIT

use std::{fmt::Debug, io};

use bytes::BytesMut;
use netlink_packet_core::{
    NetlinkBuffer,
    NetlinkDeserializable,
    NetlinkMessage,
    NetlinkSerializable,
};
pub(crate) use netlink_proto::{NetlinkCodec, NetlinkMessageCodec};

/// audit specific implementation of [`NetlinkMessageCodec`] due to the
/// protocol violations in messages generated by kernal audit.
///
/// Among the known bugs in kernel audit messages:
/// - `nlmsg_len` sometimes contains the padding too (it shouldn't)
/// - `nlmsg_len` sometimes doesn't contain the header (it really should)
///
/// See also:
/// - https://blog.des.no/2020/08/netlink-auditing-and-counting-bytes/
/// - https://github.com/torvalds/linux/blob/b5013d084e03e82ceeab4db8ae8ceeaebe76b0eb/kernel/audit.c#L2386
/// - https://github.com/mozilla/libaudit-go/issues/24
/// - https://github.com/linux-audit/audit-userspace/issues/78
pub struct NetlinkAuditCodec {
    // we don't need an instance of this, just the type
    _private: (),
}

impl NetlinkMessageCodec for NetlinkAuditCodec {
    fn decode<T>(src: &mut BytesMut) -> io::Result<Option<NetlinkMessage<T>>>
    where
        T: NetlinkDeserializable + Debug,
    {
        debug!("NetlinkAuditCodec: decoding next message");

        loop {
            // If there's nothing to read, return Ok(None)
            if src.is_empty() {
                trace!("buffer is empty");
                return Ok(None);
            }

            // This is a bit hacky because we don't want to keep `src`
            // borrowed, since we need to mutate it later.
            let src_len = src.len();
            let len = match NetlinkBuffer::new_checked(src.as_mut()) {
                Ok(mut buf) => {
                    if (src_len as isize - buf.length() as isize) <= 16 {
                        // The audit messages are sometimes truncated,
                        // because the length specified in the header,
                        // does not take the header itself into
                        // account. To workaround this, we tweak the
                        // length. We've noticed two occurences of
                        // truncated packets:
                        //
                        // - the length of the header is not included (see also:
                        //   https://github.com/mozilla/libaudit-go/issues/24)
                        // - some rule message have some padding for alignment (see
                        //   https://github.com/linux-audit/audit-userspace/issues/78) which is not
                        //   taken into account in the buffer length.
                        //
                        // How do we know that's the right length? Due to an implementation detail and to
                        // the fact that netlink is a datagram protocol.
                        //
                        // - our implementation of Stream always calls the codec with at most 1 message in
                        //   the buffer, so we know the extra bytes do not belong to another message.
                        // - because netlink is a datagram protocol, we receive entire messages, so we know
                        //   that if those extra bytes do not belong to another message, they belong to
                        //   this one.
                        warn!("found what looks like a truncated audit packet");
                        // also write correct length to buffer so parsing does not fail:
                        warn!(
                            "setting packet length to {} instead of {}",
                            src_len,
                            buf.length()
                        );
                        buf.set_length(src_len as u32);
                        src_len
                    } else {
                        buf.length() as usize
                    }
                }
                Err(e) => {
                    // We either received a truncated packet, or the
                    // packet if malformed (invalid length field). In
                    // both case, we can't decode the datagram, and we
                    // cannot find the start of the next one (if
                    // any). The only solution is to clear the buffer
                    // and potentially lose some datagrams.
                    error!(
                        "failed to decode datagram, clearing buffer: {:?}: {:#x?}.",
                        e,
                        src.as_ref()
                    );
                    src.clear();
                    return Ok(None);
                }
            };

            let bytes = src.split_to(len);

            let parsed = NetlinkMessage::<T>::deserialize(&bytes);
            match parsed {
                Ok(packet) => {
                    trace!("<<< {:?}", packet);
                    return Ok(Some(packet));
                }
                Err(e) => {
                    error!("failed to decode packet {:#x?}: {}", &bytes, e);
                    // continue looping, there may be more datagrams in the buffer
                }
            }
        }
    }

    fn encode<T>(msg: NetlinkMessage<T>, buf: &mut BytesMut) -> io::Result<()>
    where
        T: Debug + NetlinkSerializable,
    {
        NetlinkCodec::encode(msg, buf)
    }
}
