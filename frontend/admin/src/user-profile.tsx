import { useEffect, useState } from 'react';
import { Avatar, Space } from 'antd';
import { UserOutlined } from '@ant-design/icons';
import { useNavigate } from 'react-router-dom';
import { api, resolveApiUrl } from './api';
import type { UserProfile } from './types';

// 模块级缓存：头像/用户名变化频率极低（换头像才会变），全站每 uid 只拉一次；
// 404（该 uid 尚未同步过，后端没有其资料）与其它错误同样静默回退 null
const cache = new Map<string, UserProfile | null>();
const inflight = new Map<string, Promise<UserProfile | null>>();

function loadProfile(uid: string): Promise<UserProfile | null> {
  if (cache.has(uid)) return Promise.resolve(cache.get(uid)!);
  let p = inflight.get(uid);
  if (!p) {
    p = api
      .getUserProfile(uid)
      .catch(() => null)
      .then((v) => {
        cache.set(uid, v);
        inflight.delete(uid);
        return v;
      });
    inflight.set(uid, p);
  }
  return p;
}

/** 用户资料（头像 + 用户名）；uid 为空或后端尚未同步过该用户时为 null */
export function useUserProfile(uid: string | null | undefined): UserProfile | null {
  const [profile, setProfile] = useState<UserProfile | null>(() =>
    uid ? (cache.get(uid) ?? null) : null,
  );
  useEffect(() => {
    if (!uid) {
      setProfile(null);
      return;
    }
    let alive = true;
    loadProfile(uid).then((p) => {
      if (alive) setProfile(p);
    });
    return () => {
      alive = false;
    };
  }, [uid]);
  return profile;
}

/** 用户头像图：经本地封面代理加载（豆瓣头像域浏览器直连 418）；有用户名但
 *  无头像或图裂时显示首字，无资料时回退图标——rc-avatar 的回退顺序是
 *  icon 先于 children，icon 恒传会吞掉首字，故仅在无用户名时传入。 */
export function UserAvatar({ uid, size = 24 }: { uid: string | null | undefined; size?: number }) {
  const profile = useUserProfile(uid);
  return (
    <Avatar
      size={size}
      src={profile?.avatar ? resolveApiUrl(profile.avatar) : undefined}
      icon={profile?.username ? undefined : <UserOutlined />}
    >
      {profile?.username ? profile.username.slice(0, 1) : undefined}
    </Avatar>
  );
}

/** 用户列通用渲染：头像 + 昵称（该 uid 尚未同步过时回退 uid）+ @uid，点击跳数据浏览。
 *  用户页 / 任务表 / 概览订阅表与最近任务共用。 */
export function UserCell({ uid, size = 20 }: { uid: string; size?: number }) {
  const navigate = useNavigate();
  const profile = useUserProfile(uid);
  return (
    <Space size={6}>
      <UserAvatar uid={uid} size={size} />
      <a onClick={() => navigate(`/marks?uid=${encodeURIComponent(uid)}`)}>
        {profile?.username || uid}
      </a>
      {profile && <span style={{ fontSize: 12, opacity: 0.55 }}>@{uid}</span>}
    </Space>
  );
}
